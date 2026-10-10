//! Adds support for the IMAP IDLE command specificed in [RFC
//! 2177](https://tools.ietf.org/html/rfc2177).

use crate::client::Session;
use crate::error::{Error, Result};
#[cfg(feature = "tls")]
use native_tls::TlsStream;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// `Handle` allows a client to block waiting for changes to the remote mailbox.
///
/// The handle blocks using the [`IDLE` command](https://tools.ietf.org/html/rfc2177#section-3)
/// specificed in [RFC 2177](https://tools.ietf.org/html/rfc2177) until the underlying server state
/// changes in some way. While idling does inform the client what changes happened on the server,
/// this implementation will currently just block until _anything_ changes, and then notify the
///
/// Note that the server MAY consider a client inactive if it has an IDLE command running, and if
/// such a server has an inactivity timeout it MAY log the client off implicitly at the end of its
/// timeout period.  Because of that, clients using IDLE are advised to terminate the IDLE and
/// re-issue it at least every 29 minutes to avoid being logged off. [`Handle::wait_keepalive`]
/// does this. This still allows a client to receive immediate mailbox updates even though it need
/// only "poll" at half hour intervals.
///
/// As long as a [`Handle`] is active, the mailbox cannot be otherwise accessed.
#[derive(Debug)]
pub struct Handle<'a, T: Read + Write> {
    session: &'a mut Session<T>,
    keepalive: Duration,
    done: bool,
}

/// The result of a wait on a [`Handle`]
#[derive(Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The wait timed out
    TimedOut,
    /// The mailbox was modified
    MailboxChanged,
}

/// Must be implemented for a transport in order for a `Session` using that transport to support
/// operations with timeouts.
///
/// Examples of where this is useful is for `Handle::wait_keepalive` and
/// `Handle::wait_timeout`.
pub trait SetReadTimeout {
    /// Set the timeout for subsequent reads to the given one.
    ///
    /// If `timeout` is `None`, the read timeout should be removed.
    ///
    /// See also `std::net::TcpStream::set_read_timeout`.
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<()>;
}

/// IDLE 生命周期关键节点的调试日志（`PONY_IMAP_DEBUG` 开启时输出到 stderr）。
///
/// 配合 client.rs 带时间戳的 S:/C: 协议日志，用于定位「新邮件到达慢在哪个环节」：
/// 服务器推送 → DONE 收尾 → （调用方）SEARCH/FETCH。
fn debug_log(msg: &str) {
    if std::env::var_os("PONY_IMAP_DEBUG").is_some() {
        eprintln!("[idle {}] {msg}", chrono::Local::now().format("%H:%M:%S%.3f"));
    }
}

impl<'a, T: Read + Write + 'a> Handle<'a, T> {
    pub(crate) fn make(session: &'a mut Session<T>) -> Result<Self> {
        let mut h = Handle {
            session,
            keepalive: Duration::from_secs(29 * 60),
            done: false,
        };
        h.init()?;
        Ok(h)
    }

    fn init(&mut self) -> Result<()> {
        // https://tools.ietf.org/html/rfc2177
        //
        // The IDLE command takes no arguments.
        //
        // 先标记 done：从发出 IDLE 起，任何失败路径都不再走 Drop 里的 DONE 收尾——
        // 那条路径的读没有超时，对端异常时会永久阻塞；失败会话由调用方整体丢弃重连。
        // 成功进入 IDLE 时会在下面把 done 置回 false。
        self.done = true;
        self.session.run_command("IDLE")?;
        let t0 = std::time::Instant::now();
        self.wait_continuation()?;
        debug_log(&format!("已进入 IDLE（等待续行 {:?}）", t0.elapsed()));
        self.done = false;
        Ok(())
    }

    /// 等待 IDLE 的续行 `+ idling`。
    ///
    /// 续行前可能夹带不受控的 untagged 行（如 Dovecot 的 `* OK Still here`、
    /// 恰逢其时的新邮件通知），逐行读直到看见续行。
    ///
    /// 若读到 tagged 响应（如 `tag NO ...`），说明服务器拒绝了 IDLE。
    /// 上游这里直接 `unreachable!()`——本仓库 release 构建 panic=abort，
    /// 一旦触发会崩掉整个应用；改为返回错误，让调用方断开重连。
    fn wait_continuation(&mut self) -> Result<()> {
        loop {
            let mut line = Vec::new();
            self.session.readline(&mut line)?;
            if line.starts_with(b"+") {
                return Ok(());
            }
            if let Ok((_, imap_proto::Response::Done { .. })) = imap_proto::parse_response(&line)
            {
                debug_log(&format!(
                    "服务器拒绝 IDLE：{}",
                    String::from_utf8_lossy(&line).trim_end()
                ));
                return Err(Error::Bad("服务器拒绝 IDLE 命令".to_string()));
            }
            debug_log(&format!(
                "续行前收到 untagged 行，继续等待：{}",
                String::from_utf8_lossy(&line).trim_end()
            ));
        }
    }

    fn terminate(&mut self) -> Result<()> {
        if !self.done {
            self.done = true;
            self.session.write_line(b"DONE")?;
            self.session.read_response().map(|_| ())
        } else {
            Ok(())
        }
    }

    /// Internal helper that doesn't consume self.
    ///
    /// This is necessary so that we can keep using the inner `Session` in `wait_keepalive`.
    fn wait_inner(&mut self, reconnect: bool) -> Result<WaitOutcome> {
        let mut v = Vec::new();
        loop {
            let result = match self.session.readline(&mut v).map(|_| ()) {
                Err(Error::Io(ref e))
                    if e.kind() == io::ErrorKind::TimedOut
                        || e.kind() == io::ErrorKind::WouldBlock =>
                {
                    if reconnect {
                        self.terminate()?;
                        self.init()?;
                        return self.wait_inner(reconnect);
                    }
                    debug_log("等待超时（服务器未推送变化），退出 IDLE 重新确认状态");
                    Ok(WaitOutcome::TimedOut)
                }
                Ok(()) => {
                    debug_log(&format!(
                        "收到服务器推送，退出 IDLE：{}",
                        String::from_utf8_lossy(&v).trim_end()
                    ));
                    Ok(WaitOutcome::MailboxChanged)
                }
                Err(r) => {
                    debug_log(&format!("IDLE 等待出错：{r}"));
                    Err(r)
                }
            }?;

            // Handle Dovecot's imap_idle_notify_interval message
            if v.eq_ignore_ascii_case(b"* OK Still here\r\n") {
                v.clear();
            } else {
                break Ok(result);
            }
        }
    }

    /// Block until the selected mailbox changes.
    pub fn wait(mut self) -> Result<()> {
        self.wait_inner(true).map(|_| ())
    }
}

impl<'a, T: SetReadTimeout + Read + Write + 'a> Handle<'a, T> {
    /// Set the keep-alive interval to use when `wait_keepalive` is called.
    ///
    /// The interval defaults to 29 minutes as dictated by RFC 2177.
    pub fn set_keepalive(&mut self, interval: Duration) {
        self.keepalive = interval;
    }

    /// Block until the selected mailbox changes.
    ///
    /// This method differs from [`Handle::wait`] in that it will periodically refresh the IDLE
    /// connection, to prevent the server from timing out our connection. The keepalive interval is
    /// set to 29 minutes by default, as dictated by RFC 2177, but can be changed using
    /// [`Handle::set_keepalive`].
    ///
    /// This is the recommended method to use for waiting.
    pub fn wait_keepalive(self) -> Result<()> {
        // The server MAY consider a client inactive if it has an IDLE command
        // running, and if such a server has an inactivity timeout it MAY log
        // the client off implicitly at the end of its timeout period.  Because
        // of that, clients using IDLE are advised to terminate the IDLE and
        // re-issue it at least every 29 minutes to avoid being logged off.
        // This still allows a client to receive immediate mailbox updates even
        // though it need only "poll" at half hour intervals.
        let keepalive = self.keepalive;
        self.timed_wait(keepalive, true).map(|_| ())
    }

    /// Block until the selected mailbox changes, or until the given amount of time has expired.
    #[deprecated(note = "use wait_with_timeout instead")]
    pub fn wait_timeout(self, timeout: Duration) -> Result<()> {
        self.wait_with_timeout(timeout).map(|_| ())
    }

    /// Block until the selected mailbox changes, or until the given amount of time has expired.
    pub fn wait_with_timeout(self, timeout: Duration) -> Result<WaitOutcome> {
        self.timed_wait(timeout, false)
    }

    /// 带读超时的 DONE 收尾：退出 IDLE 并等服务器的 tagged 响应。
    ///
    /// 上游把这次收尾留给 `Drop`，而 `Drop` 路径的读**没有超时**——
    /// 连接被对端静默丢弃（如网络断开）时会永久阻塞监听线程。
    fn terminate_timed(&mut self, timeout: Duration) -> Result<()> {
        if self.done {
            return Ok(());
        }
        self.done = true;
        let t0 = std::time::Instant::now();
        self.session.write_line(b"DONE")?;
        self.session.stream.get_mut().set_read_timeout(Some(timeout))?;
        let res = self.session.read_response().map(|_| ());
        let _ = self.session.stream.get_mut().set_read_timeout(None).is_ok();
        match &res {
            Ok(()) => debug_log(&format!("DONE 收尾完成（{:?}）", t0.elapsed())),
            Err(e) => debug_log(&format!("DONE 收尾失败（{:?}）：{e}", t0.elapsed())),
        }
        res
    }

    fn timed_wait(mut self, timeout: Duration, reconnect: bool) -> Result<WaitOutcome> {
        self.session
            .stream
            .get_mut()
            .set_read_timeout(Some(timeout))?;
        let res = self.wait_inner(reconnect);
        let _ = self.session.stream.get_mut().set_read_timeout(None).is_ok();
        // 收尾放在这里（而不是 Drop），保证 DONE 的响应读取有超时兜底。
        self.terminate_timed(Duration::from_secs(10))?;
        res
    }
}

impl<'a, T: Read + Write + 'a> Drop for Handle<'a, T> {
    fn drop(&mut self) {
        // we don't want to panic here if we can't terminate the Idle
        let _ = self.terminate().is_ok();
    }
}

impl<'a> SetReadTimeout for TcpStream {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<()> {
        TcpStream::set_read_timeout(self, timeout).map_err(Error::Io)
    }
}

#[cfg(feature = "tls")]
impl<'a> SetReadTimeout for TlsStream<TcpStream> {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.get_ref().set_read_timeout(timeout).map_err(Error::Io)
    }
}
