// Copyright (c) 2019 Cloudflare, Inc. All rights reserved.
// SPDX-License-Identifier: BSD-3-Clause

use parking_lot::RwLock;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use crate::device::{AllowedIps, Error, MakeExternalBoringtun};
use crate::noise::{Tunn, TunnResult};

use crate::device::udp::UDPSocket;

#[derive(Default, Debug)]
pub struct Endpoint {
    pub addr: Option<SocketAddr>,
    pub conn: Option<Arc<UDPSocket>>,
}

pub struct Peer {
    /// The associated tunnel struct
    pub(crate) tunnel: Tunn,
    /// The index the tunnel uses
    index: u32,
    endpoint: RwLock<Endpoint>,
    allowed_ips: RwLock<AllowedIps<()>>,
    preshared_key: RwLock<Option<[u8; 32]>>,
    protect: Arc<dyn MakeExternalBoringtun>,
}

#[derive(Debug)]
pub struct AllowedIP {
    pub addr: IpAddr,
    pub cidr: u8,
}

impl FromStr for AllowedIP {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let ip: Vec<&str> = s.split('/').collect();
        if ip.len() != 2 {
            return Err("Invalid IP format".to_owned());
        }

        let (addr, cidr) = (ip[0].parse::<IpAddr>(), ip[1].parse::<u8>());
        match (addr, cidr) {
            (Ok(addr @ IpAddr::V4(_)), Ok(cidr)) if cidr <= 32 => Ok(AllowedIP { addr, cidr }),
            (Ok(addr @ IpAddr::V6(_)), Ok(cidr)) if cidr <= 128 => Ok(AllowedIP { addr, cidr }),
            _ => Err("Invalid IP format".to_owned()),
        }
    }
}

impl Peer {
    pub fn new(
        tunnel: Tunn,
        index: u32,
        endpoint: Option<SocketAddr>,
        allowed_ips: &[AllowedIP],
        preshared_key: Option<[u8; 32]>,
        protect: Arc<dyn MakeExternalBoringtun>,
    ) -> Peer {
        Peer {
            tunnel,
            index,
            endpoint: RwLock::new(Endpoint {
                addr: endpoint,
                conn: None,
            }),
            allowed_ips: RwLock::new(allowed_ips.iter().map(|ip| (ip, ())).collect()),
            preshared_key: RwLock::new(preshared_key),
            protect,
        }
    }

    pub fn update_timers<'a>(&mut self, dst: &'a mut [u8]) -> TunnResult<'a> {
        self.tunnel.update_timers(dst)
    }

    pub fn endpoint(&self) -> parking_lot::RwLockReadGuard<'_, Endpoint> {
        self.endpoint.read()
    }

    pub fn shutdown_endpoint(&self) {
        if let Some(conn) = self.endpoint.write().conn.take() {
            tracing::info!("Disconnecting from endpoint");
            conn.shutdown();
        }
    }

    pub fn set_endpoint(&self, addr: SocketAddr) {
        let mut endpoint = self.endpoint.write();
        if endpoint.addr != Some(addr) {
            // We only need to update the endpoint if it differs from the current one
            if let Some(conn) = endpoint.conn.take() {
                conn.shutdown();
            }

            *endpoint = Endpoint {
                addr: Some(addr),
                conn: None,
            }
        };
    }

    pub fn connect_endpoint(
        &self,
        port: u16,
        fwmark: Option<u32>,
    ) -> Result<Arc<UDPSocket>, Error> {
        let mut endpoint = self.endpoint.write();

        if endpoint.conn.is_some() {
            return Err(Error::Connect("Connected".to_owned()));
        }

        let socket = match endpoint.addr {
            Some(_addr @ SocketAddr::V4(_)) => UDPSocket::new(self.protect.clone())?,
            Some(_addr @ SocketAddr::V6(_)) => UDPSocket::new6(self.protect.clone())?,
            None => panic!("Attempt to connect to undefined endpoint"),
        };

        if let Some(fwmark) = fwmark {
            socket.set_fwmark(fwmark)?;
        }

        let udp_conn = Arc::new(
            socket
                .set_non_blocking()?
                .set_reuse()?
                .bind(port)?
                .connect(&endpoint.addr.unwrap())?,
        );

        tracing::info!(
            message="Connected endpoint",
            port=port,
            endpoint=?endpoint.addr.unwrap()
        );

        endpoint.conn = Some(Arc::clone(&udp_conn));

        Ok(udp_conn)
    }

    pub fn is_allowed_ip<I: Into<IpAddr>>(&self, addr: I) -> bool {
        self.allowed_ips.read().find(addr.into()).is_some()
    }

    pub fn allowed_ips(&self) -> Vec<AllowedIP> {
        self.allowed_ips
            .read()
            .iter()
            .map(|(_, ip, cidr)| AllowedIP {
                addr: ip,
                cidr: cidr,
            })
            .collect()
    }

    pub fn add_allowed_ips(&self, new_allowed_ips: &[AllowedIP]) {
        let mut allowed_ips = self.allowed_ips.write();

        for AllowedIP { addr, cidr } in new_allowed_ips {
            allowed_ips.insert(*addr, *cidr as u32, ());
        }
    }

    pub fn set_allowed_ips(&self, allowed_ips: &[AllowedIP]) {
        *self.allowed_ips.write() = allowed_ips.iter().map(|ip| (ip, ())).collect();
    }

    pub fn time_since_last_handshake(&self) -> Option<std::time::Duration> {
        self.tunnel.time_since_last_handshake()
    }

    pub fn persistent_keepalive(&self) -> Option<u16> {
        self.tunnel.persistent_keepalive()
    }

    pub fn set_persistent_keepalive(&self, keepalive: u16) {
        self.tunnel.set_persistent_keepalive(keepalive);
    }

    pub fn preshared_key(&self) -> Option<[u8; 32]> {
        *self.preshared_key.read()
    }

    pub fn set_preshared_key(&self, key: [u8; 32]) {
        let mut preshared_key = self.preshared_key.write();

        let _ = preshared_key.replace(key);
    }

    pub fn index(&self) -> u32 {
        self.index
    }
}
