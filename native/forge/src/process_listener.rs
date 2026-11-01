//! Same-UID process identity for an exact loopback listening socket.
use anyhow::{ensure, Context, Result};
use std::{
    net::{IpAddr, SocketAddr},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

fn kernel_address(address: SocketAddr) -> String {
    let host = match address.ip() {
        IpAddr::V4(ip) => format!("{:08X}", u32::from_ne_bytes(ip.octets())),
        IpAddr::V6(ip) => ip
            .octets()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|part| format!("{:08X}", u32::from_ne_bytes(*part)))
            .collect(),
    };
    format!("{host}:{:04X}", address.port())
}

pub fn owned(address: SocketAddr, pid: u32) -> Result<u64> {
    ensure!(
        address.ip().is_loopback() && pid > 0,
        "listener identity requires loopback and a positive PID"
    );
    let proc = PathBuf::from(format!("/proc/{pid}"));
    ensure!(
        std::fs::metadata(&proc)?.uid() == unsafe { libc::geteuid() },
        "server PID belongs to another user"
    );
    let mut inodes = Vec::new();
    let expected = kernel_address(address);
    for table_path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let table = crate::bounded_file::read(Path::new(table_path), 4 * 1024 * 1024)?;
        for row in std::str::from_utf8(&table)?.lines().skip(1) {
            let fields: Vec<_> = row.split_whitespace().collect();
            if fields.len() < 10
                || fields[3] != "0A"
                || !fields[1].ends_with(&format!(":{:04X}", address.port()))
            {
                continue;
            }
            ensure!(
                fields[1] == expected,
                "dedicated port has an ambiguous listener"
            );
            inodes.push(fields[9].to_owned());
        }
    }
    ensure!(
        inodes.len() == 1,
        "dedicated port must have exactly one loopback listener"
    );
    for (count, entry) in std::fs::read_dir(proc.join("fd"))?.enumerate() {
        ensure!(count < 8192, "server descriptor inventory exceeds limit");
        let link = match std::fs::read_link(entry?.path()) {
            Ok(link) => link,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        if inodes
            .iter()
            .any(|inode| link == Path::new(&format!("socket:[{inode}]")))
        {
            let stat = crate::bounded_file::read(&proc.join("stat"), 8192)?;
            return Ok(std::str::from_utf8(&stat)?
                .rsplit_once(')')
                .context("malformed server process stat")?
                .1
                .split_whitespace()
                .nth(19)
                .context("server start time missing")?
                .parse()?);
        }
    }
    anyhow::bail!("specified server PID does not own selected listener")
}

#[cfg(test)]
mod tests {
    #[test]
    fn listener_identity_distinguishes_owner_from_unrelated_process() {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        assert!(super::owned(address, std::process::id()).unwrap() > 0);
        assert!(super::owned(address, 1).is_err());
        drop(socket);
        assert!(super::owned(address, std::process::id()).is_err());
    }
}
