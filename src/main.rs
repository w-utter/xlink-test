#[tokio::main]
async fn main() {
    let ctx = SearchCtx::new().await.unwrap();
    let devs = ctx.search_ip(SearchParams::default()).await.unwrap();
    println!("found devices: {devs:?}");

    for dev in devs {
        let stream = dev.connect().await.unwrap();
    }
}

#[derive(Default)]
struct SearchParams {
    pub addr: Option<std::net::IpAddr>,
    pub mxid: Option<[u8; 32]>,
    pub expected_devices: usize,
    pub device_state: Option<DeviceState>,
}

struct SearchCtx {
    broadcast_sock: tokio::net::UdpSocket,
}

impl SearchCtx {
    async fn new() -> std::io::Result<Self> {
        let broadcast_sock = tokio::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0)).await?;
        broadcast_sock.set_broadcast(true)?;

        Ok(Self {
            broadcast_sock
        })
    }
    const BROADCAST_PORT: u16 = 11491;
    const DEVICE_DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2000);

    async fn search_ip(&self, params: SearchParams) -> std::io::Result<Vec<IpDevice>> {
        if let Some(target) = params.addr {
            let cmd = HostCommand::DeviceDiscover as u8;
            let send_buffer = bytemuck::bytes_of(&cmd);
            self.broadcast_sock.send_to(send_buffer, (target, Self::BROADCAST_PORT)).await?;
        }

        if params.addr.is_none() || (params.addr.is_some() && params.mxid.is_some()) {
            self.send_broadcast_ip().await?;
        }

        let mut devices = vec![];

        match tokio::time::timeout(Self::DEVICE_DISCOVERY_TIMEOUT, self.device_discovery(params.device_state.unwrap_or_default(), params.addr.as_ref(), params.mxid.as_ref(), &mut devices)).await {
            Ok(Err(e)) => return Err(e),
            // both Ok(Ok(_)) | Err(_) are good here
            _ => (),
        }

        Ok(devices)
    }

    async fn device_discovery(&self, target_state: DeviceState, target_addr: Option<&std::net::IpAddr>, target_mxid: Option<&[u8; 32]>, devices: &mut Vec<IpDevice>) -> std::io::Result<()> {
        let mut recv = DeviceDiscoveryResponseExt::default();

        loop {
            let recv_buf = bytemuck::bytes_of_mut(&mut recv);
            let (len, addr) = self.broadcast_sock.recv_from(recv_buf).await?;

            if len != core::mem::size_of::<DeviceDiscoveryResponse>() {
                continue;
            }

            if !recv.valid_device_discovery(target_state) {
                continue;
            }

            if let Some(target_addr) = target_addr && addr.ip() != *target_addr {
                continue;
            }

            if let Some(mxid) = target_mxid && recv.mxid != *mxid {
                continue;
            }

            let device = match recv.host_command() {
                Some(HostCommand::DeviceDiscover) => {
                    IpDevice {
                        addr: addr.ip(),
                        mxid: recv.mxid,
                        state: target_state,
                        port: None,
                    }
                }
                Some(HostCommand::DeviceDiscoveryEx) if len < core::mem::size_of::<DeviceDiscoveryResponseExt>() => continue,
                Some(HostCommand::DeviceDiscoveryEx) => {
                    IpDevice {
                        addr: addr.ip(),
                        mxid: recv.mxid,
                        state: target_state,
                        port: Some(recv.port_http)
                    }
                }
                _ => continue,
            };
            devices.push(device);
        }
    }

    async fn send_broadcast_ip(&self) -> std::io::Result<()> {
        let cmd = HostCommand::DeviceDiscover as u8;
        let send_buffer = bytemuck::bytes_of(&cmd);

        // send to all network interfaces
        for itf in getifaddrs::getifaddrs()? {
            use getifaddrs::{InterfaceFlags, Address};

            if !itf.flags.contains(InterfaceFlags::UP | InterfaceFlags::RUNNING) {
                continue;
            }

            match itf.address {
                Address::V4(ipv4) => {
                    let netmask = ipv4.netmask.unwrap_or(std::net::Ipv4Addr::UNSPECIFIED);
                    let addr = ipv4.address & !netmask;

                    let _ = self.broadcast_sock.send_to(send_buffer, (addr, Self::BROADCAST_PORT)).await;
                }
                Address::V6(ipv6) => {
                    let netmask = ipv6.netmask.unwrap_or(std::net::Ipv6Addr::UNSPECIFIED);
                    let addr = ipv6.address & !netmask;

                    let _ = self.broadcast_sock.send_to(send_buffer, (addr, Self::BROADCAST_PORT)).await;
                }
                Address::Mac(_) => continue,
            }
        }

        // ipv4 broadcast
        let _ = self.broadcast_sock.send_to(send_buffer, (std::net::Ipv4Addr::BROADCAST, Self::BROADCAST_PORT)).await;
        Ok(())
    }
}

#[derive(Debug)]
struct IpDevice {
    addr: std::net::IpAddr,
    mxid: [u8; 32],
    state: DeviceState,
    port: Option<u16>,
}

impl IpDevice {
    const SOCKET_PORT: u16 = 11490;
    async fn connect(&self) -> std::io::Result<tokio::net::TcpStream> {
        let sock = tokio::net::TcpSocket::new_v4()?;
        sock.set_reuseaddr(true)?;
        sock.set_nodelay(true)?;

        let port = self.port.unwrap_or(Self::SOCKET_PORT);
        let stream = sock.connect((self.addr, port).into()).await?;

        Ok(stream)
    }
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum HostCommand {
    NoCommand = 0,
    DeviceDiscover = 1,
    DeviceInfo = 2,
    Reset = 3,
    DeviceDiscoveryEx = 4,
}

impl TryFrom<u8> for HostCommand {
    type Error = u8;
    fn try_from(f: u8) -> Result<Self, Self::Error> {
        Ok(match f {
            0 => Self::NoCommand,
            1 => Self::DeviceDiscover,
            2 => Self::DeviceInfo,
            3 => Self::Reset,
            4 => Self::DeviceDiscoveryEx,
            o => return Err(o)
        })
    }
}

#[derive(Clone, Copy, Default, bytemuck::Zeroable)]
#[repr(C)]
struct DeviceDiscoveryResponse {
    command: u8,
    mxid: [u8; 32],
    state: u32,
    // extended fields
    protocol: u32,
    platform: u32,
    port_http: u16,
    port_https: u16,
}

unsafe impl bytemuck::Pod for DeviceDiscoveryResponse {}


#[derive(Clone, Copy, Default, bytemuck::Zeroable)]
#[repr(C)]
struct DeviceDiscoveryResponseExt {
    command: u8,
    mxid: [u8; 32],
    state: u32,
    // extended fields
    protocol: u32,
    platform: u32,
    port_http: u16,
    port_https: u16,
}

unsafe impl bytemuck::Pod for DeviceDiscoveryResponseExt {}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
enum DeviceState {
    #[default]
    Any,
    Booted,
    Bootloader,
    FlashBooted,
}

enum IpDeviceState {
    Booted = 1,
    Bootloader = 3,
    FlashBooted = 4,
}

impl DeviceDiscoveryResponseExt {
    fn device_state(&self) -> DeviceState {
        match self.state {
            1 => DeviceState::Booted,
            3 => DeviceState::Bootloader,
            4 => DeviceState::FlashBooted,
            _ => DeviceState::Any,
        }
    }

    fn host_command(&self) -> Option<HostCommand> {
        HostCommand::try_from(self.command).ok()
    }

    fn valid_device_discovery(&self, target: DeviceState) -> bool {
        let device_state = self.device_state();
        (self.command == HostCommand::DeviceDiscover as u8) && (matches!(device_state, DeviceState::Any) || device_state == target)
    }
}
