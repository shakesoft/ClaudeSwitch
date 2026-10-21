//! Generic VPN leak checks and a kill switch, std only.

use std::io::{self, Error, ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(4);
const MAGIC: u32 = 0x2112_A442;

/// Public IP and country as Cloudflare sees a plain-HTTP request made over
/// IPv4 or IPv6. (Plain HTTP because std has no TLS; cloudflare.com serves
/// the trace without redirecting, 1.1.1.1 does not.)
pub fn http_trace(v6: bool) -> io::Result<(String, String)> {
    let addr = ("cloudflare.com", 80)
        .to_socket_addrs()?
        .find(|a| a.is_ipv6() == v6)
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "无可用地址"))?;
    let mut s = TcpStream::connect_timeout(&addr, TIMEOUT)?;
    s.set_read_timeout(Some(TIMEOUT))?;
    s.write_all(b"GET /cdn-cgi/trace HTTP/1.1\r\nHost: cloudflare.com\r\nConnection: close\r\n\r\n")?;
    let mut body = String::new();
    s.read_to_string(&mut body)?;
    let field = |k: &str| body.lines().find_map(|l| l.strip_prefix(k)).map(str::to_string);
    match (field("ip="), field("loc=")) {
        (Some(ip), Some(loc)) => Ok((ip, loc)),
        _ => Err(Error::new(ErrorKind::InvalidData, body.lines().next().unwrap_or("空响应").to_string())),
    }
}

/// The address a STUN server sees our UDP packets come from (what WebRTC
/// would expose). Hand-built RFC 5389 Binding Request.
pub fn stun(server: &str) -> io::Result<SocketAddr> {
    let target = server
        .to_socket_addrs()?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "无 IPv4 地址"))?;
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_read_timeout(Some(TIMEOUT))?;
    let mut req = [0u8; 20];
    req[0..2].copy_from_slice(&1u16.to_be_bytes());
    req[4..8].copy_from_slice(&MAGIC.to_be_bytes());
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    req[8..20].copy_from_slice(&seed.to_be_bytes()[4..16]);
    sock.send_to(&req, target)?;
    let mut buf = [0u8; 512];
    let n = sock.recv(&mut buf)?;
    parse_mapped(&buf[..n]).ok_or_else(|| Error::new(ErrorKind::InvalidData, "STUN 响应无法解析"))
}

/// Extracts XOR-MAPPED-ADDRESS (or plain MAPPED-ADDRESS) for IPv4.
fn parse_mapped(msg: &[u8]) -> Option<SocketAddr> {
    let mut i = 20;
    while i + 4 <= msg.len() {
        let kind = u16::from_be_bytes([msg[i], msg[i + 1]]);
        let len = u16::from_be_bytes([msg[i + 2], msg[i + 3]]) as usize;
        let v = msg.get(i + 4..i + 4 + len)?;
        if (kind == 0x0020 || kind == 0x0001) && len >= 8 && v[1] == 0x01 {
            let mut port = u16::from_be_bytes([v[2], v[3]]);
            let mut ip = u32::from_be_bytes([v[4], v[5], v[6], v[7]]);
            if kind == 0x0020 {
                port ^= (MAGIC >> 16) as u16;
                ip ^= MAGIC;
            }
            return Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port));
        }
        i += 4 + len.div_ceil(4) * 4;
    }
    None
}

fn powershell(script: &str, env: &[(&str, &str)]) -> io::Result<String> {
    // Force UTF-8 output; the default OEM code page garbles non-ASCII adapter names.
    let script = format!("[Console]::OutputEncoding = [Text.Encoding]::UTF8; {script}");
    let out = Command::new("powershell.exe").args(["-NoProfile", "-Command", &script]).envs(env.iter().copied()).output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// "adapter: server, server" per adapter that has IPv4 DNS servers.
pub fn dns_servers() -> io::Result<String> {
    powershell(
        "Get-DnsClientServerAddress -AddressFamily IPv4 | Where-Object ServerAddresses | \
         ForEach-Object { $_.InterfaceAlias + ': ' + ($_.ServerAddresses -join ', ') }",
        &[],
    )
}

/// Whether an adapter whose name or description contains `keyword` is up.
/// ponytail: spawns PowerShell per poll (~0.3s CPU); call GetAdaptersAddresses if that ever matters.
pub fn adapter_up(keyword: &str) -> io::Result<bool> {
    // The keyword goes in through the environment so it is never parsed as script.
    let n = powershell(
        "@(Get-NetAdapter | Where-Object { $_.Status -eq 'Up' -and \
         ($_.Name -like \"*$env:KS_KEYWORD*\" -or $_.InterfaceDescription -like \"*$env:KS_KEYWORD*\") }).Count",
        &[("KS_KEYWORD", keyword)],
    )?;
    n.parse::<u32>().map(|n| n > 0).map_err(|_| Error::new(ErrorKind::InvalidData, format!("无法读取网卡状态：{n}")))
}

/// Force-kills each image name; returns the ones that were actually running.
pub fn kill(names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|n| Command::new("taskkill").args(["/F", "/IM", n]).output().is_ok_and(|o| o.status.success()))
        .cloned()
        .collect()
}

/// Runs every check and returns display lines; warnings start with "⚠".
pub fn run_all() -> Vec<String> {
    let mut lines = Vec::new();
    let mut warnings = Vec::new();
    let mut stun_ok = false;

    let v4 = http_trace(false);
    match &v4 {
        Ok((ip, loc)) => lines.push(format!("公网 IP（HTTP）：{ip}（{loc}）")),
        Err(e) => lines.push(format!("公网 IP（HTTP）：失败 {e}")),
    }

    for server in ["stun.l.google.com:19302", "stun.cloudflare.com:3478"] {
        match stun(server) {
            Ok(addr) => {
                stun_ok = true;
                lines.push(format!("STUN {server}：{}", addr.ip()));
                if let Ok((ip, _)) = &v4 {
                    if addr.ip().to_string() != *ip {
                        warnings.push(format!("⚠ UDP 出口 {} 与 HTTP 出口 {ip} 不同，WebRTC 可能暴露非 VPN 地址", addr.ip()));
                    }
                }
            }
            Err(_) => lines.push(format!("STUN {server}：无响应（UDP 可能被拦截）")),
        }
    }

    match http_trace(true) {
        Ok((ip, loc)) => {
            lines.push(format!("IPv6 出口：{ip}（{loc}）"));
            if let Ok((_, loc4)) = &v4 {
                if loc != *loc4 {
                    warnings.push(format!("⚠ IPv6 出口地区 {loc} 与 IPv4 出口 {loc4} 不同，IPv6 可能绕过了 VPN"));
                }
            }
        }
        Err(_) => lines.push("IPv6 出口：不可用".to_string()),
    }

    match dns_servers() {
        Ok(s) if !s.is_empty() => lines.extend(s.lines().map(|l| format!("DNS {l}"))),
        _ => lines.push("DNS 服务器：读取失败".to_string()),
    }

    if !warnings.is_empty() {
        lines.extend(warnings);
    } else if v4.is_err() {
        lines.push("结论：HTTP 出口检测失败，无法判断".to_string());
    } else if !stun_ok {
        lines.push("结论：HTTP 出口正常；UDP 被拦截，WebRTC 无法经 STUN 暴露地址".to_string());
    } else {
        lines.push("结论：HTTP 与 UDP 出口一致，未发现泄漏".to_string());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xor_mapped_address() {
        // 203.0.113.5:54321 encoded per RFC 5389.
        let port = 54321u16 ^ 0x2112;
        let ip = u32::from(Ipv4Addr::new(203, 0, 113, 5)) ^ MAGIC;
        let mut msg = vec![0x01, 0x01, 0, 12];
        msg.extend(MAGIC.to_be_bytes());
        msg.extend([0u8; 12]);
        msg.extend([0x00, 0x20, 0, 8, 0, 0x01]);
        msg.extend(port.to_be_bytes());
        msg.extend(ip.to_be_bytes());
        assert_eq!(parse_mapped(&msg), Some("203.0.113.5:54321".parse().unwrap()));
        assert_eq!(parse_mapped(&msg[..24]), None, "truncated attribute");
    }
}
