use tokio::sync::mpsc;

async fn unpack_tarball(bytes: &[u8]) -> std::io::Result<tokio_tar::Entries<std::io::Cursor<Vec<u8>>>> {
    let xc_buf = {
        let mut xc_buf = vec![];
        let mut tar = xz::read::XzDecoder::new(bytes);
        use std::io::Read;
        tar.read_to_end(&mut xc_buf)?;
        xc_buf
    };

    let mut archive = tokio_tar::Archive::new(std::io::Cursor::new(xc_buf));
    archive.entries()
}



#[tokio::main]
async fn main() {
    let bootloader_firmware = include_bytes!("firmware/depthai-bootloader-fwp-0.0.28.tar.xz");
    let device_firmware = include_bytes!("firmware/depthai-device-fwp-747b3781a390caf3e0e2e78a77f201b0fd3fc22a.tar.xz");
    // TODO: properly decompress this from the device_firmware
    let device_firmware_buf = include_bytes!("firmware/depthai-device-openvino-universal-747b3781a390caf3e0e2e78a77f201b0fd3fc22a.cmd");

    let mut bootloader_firmware_entries = unpack_tarball(bootloader_firmware).await.unwrap();

    use tokio_stream::StreamExt;
    while let Some(entry) = bootloader_firmware_entries.next().await {
        //println!("{entry:?}");
    }
    //println!("max: {}", bootloader::MAX_PACKET_SIZE);

    let mut device_firmware_entries = unpack_tarball(device_firmware).await.unwrap();

    let mut device_firmware = None;

    while let Some(entry) = device_firmware_entries.next().await {
        let Ok(entry) = entry else {
            continue;
        };
        let Ok(path) = entry.path() else {
            continue;
        };

        let path: &std::ffi::OsStr = (&*path).as_ref();
        use std::os::unix::ffi::OsStrExt;
        let path = path.as_bytes();

        if path.starts_with(b"depthai-device-openvino-universal") {
            device_firmware = Some(entry);
        }
    }

    let Some(mut device_firmware) = device_firmware else {
        panic!();
    };

    device_firmware.set_unpack_xattrs(true);
    device_firmware.set_preserve_permissions(true);

    let ctx = SearchCtx::new().await.unwrap();

    let mut devs = vec![];


    loop {
        devs = ctx.search_ip(SearchParams::default()).await.unwrap();
        if !devs.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    println!("found devices: {devs:?}");

    //let firmware = 

    //let firmware = vec![];


    for dev in devs {
        let mut connection = dev.connect().await.unwrap();
        connection.send_event(Event::ping()).await.unwrap();

        connection.wait_for_pong().await;
        println!("starting");

        connection.create_stream("__bootloader", bootloader::MAX_PACKET_SIZE).await.unwrap();
        connection.create_stream("__watchdog", 64).await.unwrap();

        connection.wait_for_stream("__bootloader").await;
        connection.wait_for_stream("__watchdog").await;

        println!("got streams: {:?}", connection.created_streams);

        connection.create_watchdog(1, std::time::Duration::from_millis(2000)).await;

        //tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        // bootloader
        let bytes = {
            let data = bootloader::request::Command::GetBootloaderType as u32;
            let data_buf = bytemuck::bytes_of(&data).to_vec();
            let write = Event::write(0, b"__bootloader", &data_buf);
            connection.send_write_event(write, data_buf).await.unwrap();
            connection.wait_for_read(0).await
        };
        let bootloader = bytemuck::from_bytes::<bootloader::response::BootloaderType>(&bytes);
        println!("bootloader: {bootloader:?}");

        let Ok(ty) = bootloader.ty() else {
            panic!()
        };

        /* the actual impl does not boot the firmware in the current config
        {
            let len = firmware.len() as u32;
            let boot_memory = bootloader::request::BootMemory::new(len, ((len - 1) / bootloader::MAX_PACKET_SIZE) + 1);
            let data_buf = bytemuck::bytes_of(&boot_memory).to_vec();
            let write = Event::write(0, b"__bootloader", &data_buf);
            connection.send_write_event(write, data_buf).await.unwrap();
            connection.send_bulk_write(firmware, 0, StreamName::new(b"__bootloader").unwrap(), XLINK_MAX_PACKET_SIZE).await;
        }
        */

        println!("booting firmware");
        {
            let len = device_firmware_buf.len() as u32;
            let boot_memory = bootloader::request::BootMemory::new(len, ((len - 1) / bootloader::MAX_PACKET_SIZE) + 1);
            let data_buf = bytemuck::bytes_of(&boot_memory).to_vec();
            let write = Event::write(0, b"__bootloader", &data_buf);
            connection.send_write_event(write, data_buf).await.unwrap();
            connection.send_bulk_write(device_firmware_buf.to_vec(), 0, StreamName::new(b"__bootloader").unwrap(), XLINK_MAX_PACKET_SIZE).await;
        }

        connection.wait_for_shutdown().await;
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        println!("shutdown");

        let mut devs = vec![];
        loop {
            devs = ctx.search_ip(SearchParams::default()).await.unwrap();
            if !devs.is_empty() {
                break;
            }
        }

        println!("{devs:?}");
        connection = dev.connect().await.unwrap();

        connection.send_event(Event::ping()).await.unwrap();
        connection.wait_for_pong().await;

        connection.create_stream("__watchdog", 64).await.unwrap();
        connection.create_stream("__rpc_main", 64).await.unwrap();
        connection.create_stream("__log", 64).await.unwrap();

        connection.wait_for_stream("__watchdog").await;
        connection.wait_for_stream("__rpc_main").await;
        connection.wait_for_stream("__log").await;

        println!("got streams: {:?}", connection.created_streams);

        connection.create_watchdog(0, std::time::Duration::from_millis(2000)).await;

        loop {

        }
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
    const DEVICE_DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

    async fn search_ip(&self, params: SearchParams) -> std::io::Result<Vec<IpDevice>> {
        if let Some(target) = params.addr {
            let cmd = HostCommand::DeviceDiscover as u32;
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

            println!("received: {len:?} - {}", core::mem::size_of::<DeviceDiscoveryResponse>());

            if len < core::mem::size_of::<DeviceDiscoveryResponse>() {
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

            let state = recv.device_state();
            let device = match recv.host_command() {
                Some(HostCommand::DeviceDiscover) if len != core::mem::size_of::<DeviceDiscoveryResponse>() => continue,
                Some(HostCommand::DeviceDiscover) => {
                    IpDevice {
                        addr: addr.ip(),
                        mxid: recv.mxid,
                        state,
                        port: None,
                    }
                }
                Some(HostCommand::DeviceDiscoveryEx) if len != core::mem::size_of::<DeviceDiscoveryResponseExt>() => continue,
                Some(HostCommand::DeviceDiscoveryEx) => {
                    IpDevice {
                        addr: addr.ip(),
                        mxid: recv.mxid,
                        state,
                        port: Some(recv.port_http)
                    }
                }
                _ => continue,
            };

            devices.push(device);
        }
    }

    async fn send_broadcast_ip(&self) -> std::io::Result<()> {
        let cmd = HostCommand::DeviceDiscover as u32;
        let send_buffer = bytemuck::bytes_of(&cmd);

        println!("sending: {send_buffer:?}");

        // send to all network interfaces
        for itf in getifaddrs::getifaddrs()? {
            use getifaddrs::{InterfaceFlags, Address};

            if !itf.flags.contains(InterfaceFlags::UP | InterfaceFlags::RUNNING) {
                continue;
            }

            match itf.address {
                Address::V4(ipv4) => {
                    let addr = if let Some(addr) = ipv4.associated_address && itf.flags.contains(InterfaceFlags::BROADCAST) {
                        addr
                    } else {
                        let netmask = ipv4.netmask.unwrap_or(std::net::Ipv4Addr::UNSPECIFIED);
                        ipv4.address & !netmask
                    };

                    let _ = self.broadcast_sock.send_to(send_buffer, (addr, Self::BROADCAST_PORT)).await;
                }
                Address::V6(ipv6) => {
                    let addr = if let Some(addr) = ipv6.associated_address && itf.flags.contains(InterfaceFlags::BROADCAST) {
                        addr
                    } else {
                        let netmask = ipv6.netmask.unwrap_or(std::net::Ipv6Addr::UNSPECIFIED);
                        ipv6.address & !netmask
                    };

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

struct IpDevice {
    addr: std::net::IpAddr,
    mxid: [u8; 32],
    state: DeviceState,
    port: Option<u16>,
}

impl IpDevice {
    fn mxid(&self) -> Option<&str> {
        let len = memchr::memchr(0, &self.mxid).unwrap_or(self.mxid.len());
        core::str::from_utf8(&self.mxid[..len]).ok()
    }
}

impl core::fmt::Debug for IpDevice {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("IpDevice")
            .field("addr", &self.addr)
            .field("mxid", &self.mxid())
            .field("state", &self.state)
            .field("port", &self.port)
            .finish()
    }
}

use std::collections::HashMap;

#[derive(Debug)]
struct ReadStreamInfo {
    read_size: u32,
    id: u32,
}

#[derive(Debug)]
struct WriteStreamInfo {
    write_size: u32,
    id: u32,
}

#[derive(Clone, Copy)]
#[repr(transparent)]
struct StreamName([u8; StreamName::MAX_LEN]);

impl core::cmp::PartialEq for StreamName {
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl core::cmp::Eq for StreamName {}

impl core::hash::Hash for StreamName {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.as_bytes().hash(state)
    }
}

use core::borrow::Borrow;

impl Borrow<[u8]> for StreamName {
    fn borrow(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl StreamName {
    fn as_bytes(&self) -> &[u8] {
        let len = memchr::memchr(0, &self.0).unwrap_or(self.0.len());
        &self.0[..len]
    }

    const MAX_LEN: usize = 52;

    fn new<N: Borrow<[u8]>>(name: &N) -> Option<Self> {
        let name = name.borrow();

        if name.len() > Self::MAX_LEN {
            panic!()
        }

        let mut header_name = [0; Self::MAX_LEN];
        header_name[..name.len()].copy_from_slice(name);

        Some(Self(header_name))
    }
}

unsafe impl bytemuck::Pod for StreamName {}
unsafe impl bytemuck::Zeroable for StreamName {}

impl core::fmt::Debug for StreamName {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let res = std::string::String::from_utf8_lossy(self.as_bytes());
        let s: &str = &*res;
        write!(f, "{s:?}")?;
        Ok(())
    }
}


// TODO: move io handling into seperate task,
// change some stuff around to work with it in existing fns

#[derive(Debug)]
struct Connection {
    //inner: tokio::net::TcpStream,
    //requested_streams: HashMap<StreamName, (WriteStreamInfo, bool)>,
    //device_requested_streams: HashMap<StreamName, ReadStreamInfo>,
    created_streams: HashMap<StreamName, (ReadStreamInfo, WriteStreamInfo)>,
    io_events: mpsc::Receiver<IoEvent>,
    device_events: mpsc::Sender<DeviceEvent>,


    next_stream_id: u32,
}

impl Connection {
    async fn send_event<T: bytemuck::Pod + bytemuck::Zeroable>(&mut self, ev: Event<T>) -> std::io::Result<()> {

        self.device_events.send(DeviceEvent::NormalEvent(ev.header)).await.unwrap();
        /*
        use tokio::io::AsyncWriteExt;



        let header_buf = bytemuck::bytes_of(&ev.header);

        self.inner.write(header_buf).await?;
        */

        Ok(())
    }

    async fn send_write_event<T: bytemuck::Pod + bytemuck::Zeroable>(&mut self, ev: Event<T>, data: Vec<u8>) -> std::io::Result<()> {
        self.device_events.send(DeviceEvent::WriteEvent(ev.header, data)).await.unwrap();
            Ok(())
    }

    async fn send_bulk_write(&mut self, data: Vec<u8>, stream_id: u32, name: StreamName, split_by: usize) {
        self.device_events.send(DeviceEvent::BulkWriteEvent{ data, stream_id, name, split_by }).await.unwrap();
    }

    /*
    fn inner(&mut self) -> &mut tokio::net::TcpStream {
        &mut self.inner
    }
    */

    async fn io_ev(&mut self) -> Option<IoEvent> {
        self.io_events.recv().await
    }

    /*
    fn requested_stream_finished(&mut self, name: &StreamName) -> Option<WriteStreamInfo> {
        let (info, acked) = self.requested_streams.get(name)?;

        if *acked {
            self.requested_streams.remove(name).map(|(info, _)| info)
        } else {
            None
        }
    }
    */

    async fn create_stream(&mut self, name: &str, write_size: u32) -> std::io::Result<()> {
        let name_bytes = name.as_bytes();

        let stream_id = self.next_stream_id;

        let ev = Event::create_stream(stream_id, &name_bytes, write_size);

        self.next_stream_id += 1;

        let stream_name = ev.header.name;

        self.device_events.send(DeviceEvent::CreateStream(ev.header)).await.unwrap();


        /*
        self.requested_streams.insert(stream_name, (WriteStreamInfo {
            write_size,
            id: stream_id,
        }, false));

        let mut read_buf = [0; 128];


        // might be better to have a background task running on the read side of the tcp stream
        // the thread should have both tx & rx for acking
        // then just send stuff over channels
        //
        // can just run the tcpstream seperately and communicate bidirectionally with channels?
        // - stuff doesnt need to be split up and everything else stays roughly the same
        // - need to have smth similar to blocking evs
        // - might as well also refactor the event stuff afterward
        
        use tokio::io::AsyncReadExt;
        loop {
            let len = self.inner.read(&mut read_buf).await.unwrap();
            let buf = &read_buf[..core::cmp::min(len, core::mem::size_of::<EventHeader>())];

            if len < core::mem::size_of::<EventHeader>() {
                continue;
            }

            let stream = bytemuck::from_bytes::<EventHeader>(buf);

            println!("while creating stream: {stream:?}");

            let Ok(ty) = EventType::try_from(stream.ty) else {
                continue;
            };

            match ty {
                EventType::CreateStreamReq => {
                    self.send_event(Event::acknowledge_stream(stream.stream_id, &stream.name.0, stream.size, stream.id)).await.unwrap();

                    if let Some(existing) = self.requested_stream_finished(&stream.name) {
                        self.created_streams.insert(stream.name, (ReadStreamInfo {
                            read_size: stream.size,
                            id: stream.stream_id,
                        }, existing));

                        if stream.name.as_bytes() == name_bytes {
                            return Ok(())
                        }
                    } else {
                        self.device_requested_streams.insert(stream.name, ReadStreamInfo {
                            read_size: stream.size,
                            id: stream.stream_id,
                        });
                    }
                }
                EventType::CreateStreamResp => {
                    if let Some(existing) = self.device_requested_streams.remove(&stream.name) {
                        let (host_requested, _) = self.requested_streams.remove(&stream.name).unwrap();

                        self.created_streams.insert(stream.name, (existing, host_requested));

                        if stream.name.as_bytes() == name_bytes {
                            return Ok(())
                        }
                    } else {
                        let (_, acked) = self.requested_streams.get_mut(&stream.name).unwrap();
                        *acked = true;
                    }
                }

                _ => continue,
            }
        }
        */

        Ok(())
    }

    async fn create_watchdog(&mut self, stream_id: u32, period: std::time::Duration) {
        self.device_events.send(DeviceEvent::CreateWatchDog(stream_id, period)).await.unwrap();
    }

    async fn wait_for_pong(&mut self) {
        loop {
            let Some(ev) = self.io_events.recv().await else {
                panic!()
            };

            match ev {
                IoEvent::Pong => return,
                o => panic!("{o:?}"),
            }
        }
    }

    async fn wait_for_stream(&mut self, name: &str) {
        if self.created_streams.get(name.as_bytes()).is_some() {
            return
        }

        loop {
            let Some(ev) = self.io_events.recv().await else {
                panic!()
            };

            match ev {
                IoEvent::CreatedStream(name, read, write) => {
                    self.created_streams.insert(name, (read, write));
                    return;
                }
                o => panic!("{o:?}")
            }
        }
    }

    async fn wait_for_read(&mut self, id: u32) -> Vec<u8> {
        loop {
            let Some(ev) = self.io_events.recv().await else {
                panic!()
            };

            match ev {
                IoEvent::DeviceRead(stream, bytes) if stream == id => {
                    self.device_events.send(DeviceEvent::ReadRelease{ stream_id: stream, size: bytes.len() as _}).await.unwrap();
                    return bytes
                }
                o => panic!("{o:?}")
            }
        }
    }

    async fn wait_for_shutdown(&mut self) {
        loop {
            let Some(ev) = self.io_events.recv().await else {
                panic!()
            };

            match ev {
                IoEvent::Shutdown => return,
                o => panic!("{o:?}"),
            }
        }
    }
}

impl IpDevice {
    const SOCKET_PORT: u16 = 11490;
    async fn connect(&self) -> std::io::Result<Connection> {
        let sock = tokio::net::TcpSocket::new_v4()?;
        sock.set_reuseaddr(true)?;
        sock.set_nodelay(true)?;
        let size = sock.send_buffer_size()?;
        println!("tcp stream send buf size: {size}");
        sock.set_send_buffer_size(bootloader::MAX_PACKET_SIZE)?;
        let size = sock.send_buffer_size()?;
        println!("set send buf size to: {size}");

        let port = self.port.unwrap_or(Self::SOCKET_PORT);
        let stream = sock.connect((self.addr, port).into()).await?;

        const EV_SIZE: usize = 16;

        let (io_ev_tx, io_ev_rx) = mpsc::channel(EV_SIZE);
        let (dev_ev_tx, dev_ev_rx) = mpsc::channel(EV_SIZE);

        tokio::task::spawn(async move {
            io_thread(stream, io_ev_tx, dev_ev_rx).await;
        });


        Ok(Connection {
            /*
            inner: stream,
            requested_streams: Default::default(),
            device_requested_streams: Default::default(),
            */
            device_events: dev_ev_tx,
            io_events: io_ev_rx,
            created_streams: Default::default(),
            next_stream_id: 0,
        })
    }

    async fn boot(self, firmware: &[u8]) -> std::io::Result<Self> {
        match self.state {
            DeviceState::Booted => Ok(self),
            DeviceState::Bootloader | DeviceState::FlashBooted => {
                use tokio::io::AsyncWriteExt;

                /*
                let mut stream = self.connect().await?;



                // get bootloader type
                {
                    use tokio::io::AsyncReadExt;
                    let req = bootloader::request::Command::GetBootloaderType as u32;
                    let bytes = bytemuck::bytes_of(&req);
                    println!("sending");
                    stream.write(&bytes).await.unwrap();

                    let mut res = [0; 8];

                    println!("reading");
                    stream.read(&mut res).await?;

                    todo!("{res:?}");
                }




                let len = firmware.len() as u32;

                let cmd = bootloader::request::BootMemory {
                    val: bootloader::request::Command::BootMemory as u32,
                    total_size: len,
                    num_packets: ((len - 1) / bootloader::MAX_PACKET_SIZE) + 1,
                };


                // send request
                {
                    let send_buf = bytemuck::bytes_of(&cmd);
                    stream.write(send_buf).await?;
                }

                for bytes in firmware.chunks(bootloader::MAX_PACKET_SIZE as usize) {
                    stream.write(bytes).await?;
                }
                */

                loop {

                }



                todo!()

                // gotta bootBootloader
                //
            }
            s => todo!("boot from {s:?}"),
        }
    }
}


#[derive(Clone, Copy)]
#[repr(C, )]
struct EventHeader {
    id: u32,
    ty: u32,
    name: StreamName,
    nsec: u32,
    sec_lsb: u32,
    sec_msb: u32,
    stream_id: u32,
    size: u32,
    flags: u32,
}

impl core::fmt::Debug for EventHeader {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let ty = EventType::try_from(self.ty);
        let time = {
            let secs = (self.sec_lsb as u64) | ((self.sec_msb as u64) << 32);
            let duration = std::time::Duration::new(secs, self.nsec);

            std::time::SystemTime::UNIX_EPOCH.checked_add(duration)
        };

        f.debug_struct("EventHeader")
            .field("id", &self.id)
            .field("ty", &ty)
            .field("name", &self.name)
            .field("time", &time)
            .field("stream_id", &self.stream_id)
            .field("size", &self.size)
            .field("flags", &self.flags)
            .finish()
    }
}

unsafe impl bytemuck::Pod for EventHeader {}
unsafe impl bytemuck::Zeroable for EventHeader {}

struct Event<T> {
    header: EventHeader,
    data: T,
}

//TODO: make this more sealed (eg. supertrait w these)
impl <T: bytemuck::Pod + bytemuck::Zeroable> Event<T> {
    fn new(data: T, name: &[u8], ty: u32, stream_id: u32, flags: u32) -> Self {
        let header = {
            // FIXME: make this propagate
            let name = StreamName::new(&name).unwrap();

            let ts = std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap_or_default();

            let size = bytemuck::bytes_of(&data).len() as u32;

            let secs = ts.as_secs();
            EventHeader {
                id: 0,
                ty,
                name,
                nsec: ts.subsec_nanos(),
                sec_lsb: secs as u32,
                sec_msb: (secs >> 32) as u32,
                stream_id,
                size,
                flags,
            }
        };

        Self {
            header,
            data,
        }
    }
}

#[derive(Clone, Copy, Default, bytemuck::Zeroable, bytemuck::Pod)]
#[repr(C)]
struct Ping;

impl Event<Ping> {
    fn ping() -> Self {
        Event::new(Ping, b"", EventType::PingReq as u32, 0, 0)
    }
}

#[derive(Clone, Copy, Default, bytemuck::Zeroable, bytemuck::Pod)]
#[repr(C)]
struct CreateStream;

impl Event<CreateStream> {
    fn create_stream<N: Borrow<[u8]>>(stream_id: u32, name: &N, write_size: u32) -> Self {
        let mut ev = Event::new(CreateStream, name.borrow(), EventType::CreateStreamReq as u32, stream_id, 0);
        ev.header.size = write_size;
        ev
    }
}

#[derive(Clone, Copy, Default, bytemuck::Zeroable, bytemuck::Pod)]
#[repr(C)]
struct AckStream;

impl Event<AckStream> {
    fn acknowledge_stream<N: Borrow<[u8]>>(stream_id: u32, name: &N, write_size: u32, id: u32) -> Self {
        let mut ev = Event::new(AckStream, name.borrow(), EventType::CreateStreamResp as u32, stream_id, 1);
        ev.header.size = write_size;
        ev.header.id = id;
        ev
    }
}

#[derive(Clone, Copy, Default, bytemuck::Zeroable, bytemuck::Pod)]
#[repr(C)]
struct AckReadRel;

impl Event<AckReadRel> {
    fn acknowledge_read_release<N: Borrow<[u8]>>(stream_id: u32, name: &N, size: u32, id: u32) -> Self {
        let mut ev = Event::new(AckReadRel, name.borrow(), EventType::ReadRelResp as u32, stream_id, 1);
        ev.header.size = size;
        ev.header.id = id;
        ev
    }
}

#[derive(Clone, Copy, Default, bytemuck::Zeroable, bytemuck::Pod)]
#[repr(C)]
struct Write;

impl Event<Write> {
    fn write<N: Borrow<[u8]>>(stream_id: u32, name: &N, data: &[u8]) -> Self {
        let mut ev = Event::new(Write, name.borrow(), EventType::WriteReq as u32, stream_id, 0);
        ev.header.size = data.len() as u32;
        ev
    }
}

#[derive(Clone, Copy, Default, bytemuck::Zeroable, bytemuck::Pod)]
#[repr(C)]
struct WriteAck;

impl Event<WriteAck> {
    fn acknowledge_write<N: Borrow<[u8]>>(stream_id: u32, name: &N, len: u32, id: u32) -> Self {
        let mut ev = Event::new(WriteAck, name.borrow(), EventType::WriteResp as u32, stream_id, 1);
        ev.header.size = len;
        ev.header.id = id;
        ev
    }
}

#[derive(Clone, Copy, Default, bytemuck::Zeroable, bytemuck::Pod)]
#[repr(C)]
struct ReadRelease;

impl Event<ReadRelease> {
    fn read_release<N: Borrow<[u8]>>(stream_id: u32, name: &N, len: u32) -> Self {
        let mut ev = Event::new(ReadRelease, name.borrow(), EventType::ReadRelReq as u32, stream_id, 1);
        ev.header.size = len;
        ev
    }
}



#[derive(Debug, Clone, Copy)]
#[repr(C)]
enum EventType {
    WriteReq,
    ReadReq,
    ReadRelReq,
    CreateStreamReq,
    CloseStreamReq,
    PingReq,
    ResetReq,

    RequestLast,

    WriteResp,
    ReadResp,
    ReadRelResp,
    CreateStreamResp,
    CloseStreamResp,
    PingResp,
    ResetResp,

    RespLast,

    IpcWriteReq,
    IpcReadReq,
    IpcCreateStreamReq,
    IpcCloseStreamReq,

    IpcWriteResp,
    IpcReadResp,
    IpcCreateStreamResp,
    IpcCloseStreamResp,

    ReadRelSpecReq,
    ReadRelSpecResp,
}

impl TryFrom<u32> for EventType {
    type Error = u32;
    fn try_from(f: u32) -> Result<Self, Self::Error> {
        Ok(match f {
            0 => Self::WriteReq,
            1 => Self::ReadReq,
            2 => Self::ReadRelReq,
            3 => Self::CreateStreamReq,
            4 => Self::CloseStreamReq,
            5 => Self::PingReq,
            6 => Self::ResetReq,
            7 => Self::RequestLast,
            8 => Self::WriteResp,
            9 => Self::ReadResp,
            10 => Self::ReadRelResp,
            11 => Self::CreateStreamResp,
            12 => Self::CloseStreamResp,
            13 => Self::PingResp,
            14 => Self::ResetResp,
            15 => Self::RespLast,

            16 => Self::IpcWriteReq,
            17 => Self::IpcReadReq,
            18 => Self::IpcCreateStreamReq,
            19 => Self::IpcCloseStreamReq,

            20 => Self::IpcWriteResp,
            21 => Self::IpcReadResp,
            22 => Self::IpcCreateStreamResp,
            23 => Self::IpcCloseStreamResp,

            24 => Self::ReadRelSpecReq,
            25 => Self::ReadRelSpecResp,
            o => return Err(o)
        })
    }
}

// NOTE: if this is sent as a u8 then it will be inconsistent
#[derive(Clone, Copy, Debug)]
#[repr(u32)]
enum HostCommand {
    NoCommand = 0,
    DeviceDiscover = 1,
    DeviceInfo = 2,
    Reset = 3,
    DeviceDiscoveryEx = 4,
}

impl TryFrom<u32> for HostCommand {
    type Error = u32;
    fn try_from(f: u32) -> Result<Self, Self::Error> {
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
    command: u32,
    mxid: [u8; 32],
    state: u32,
}

unsafe impl bytemuck::Pod for DeviceDiscoveryResponse {}


#[derive(Clone, Copy, Default, bytemuck::Zeroable)]
#[repr(C)]
struct DeviceDiscoveryResponseExt {
    command: u32,
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

        (self.command == HostCommand::DeviceDiscover as u32) && (matches!(target, DeviceState::Any) || device_state == target)
    }
}

const XLINK_MAX_PACKET_SIZE: usize = bootloader::MAX_PACKET_SIZE as usize;

// this is stored in depthai-bootloader-shared
pub mod bootloader {
    //pub const MAX_PACKET_SIZE: u32 = 5 * 1024 * 1024;
    pub const MAX_PACKET_SIZE: u32 = 1024 * 400;
    pub mod request {
        #[repr(u32)]
        #[derive(Clone, Copy)]
        pub enum Command {
            UsbRomBoot = 0,
            BootApplication = 1,
            UpdateFlash = 2,
            GetBootloaderVersion = 3,
            BootMemory = 4,
            UpdateFlashEx = 5,
            UpdateFlashEx2 = 6,
            NoOp = 7,
            GetBootloaderType = 8,
            SetBootloaderConfig = 9,
            GetBootloaderConfig = 10,
            BootloaderMemory = 11,
            GetBootloaderCommit = 12,
            UpdateFlashBootHeader = 13,
            ReadFlash = 14,
            GetApplicationDetails = 15,
            GetMemoryDetails = 16,
            IsUserBootloader = 17,
        }

        #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
        #[repr(C)]
        pub struct BootMemory {
            pub val: u32,
            pub total_size: u32,
            pub num_packets: u32,
        }

        impl BootMemory {
            pub fn new(total_size: u32, num_packets: u32) -> Self {
                Self {
                    val: Command::BootMemory as u32,
                    total_size,
                    num_packets,
                }
            }
        }
    }

    pub mod response {
        #[repr(u32)]
        #[derive(Clone, Copy, Debug)]
        pub enum Command {
            FlashComplete = 0,
            FlashStatusUpdate = 1,
            BootloaderVersion = 2,
            BootloaderType = 3,
            GetBootloaderConfig = 4,
            BootloaderMemory = 5,
            BootApplication = 6,
            BootloaderCommit = 7,
            ReadFlash = 8,
            ApplicationDetails = 9,
            MemoryDetails = 10,
            IsUserBootloader = 11,
            NoOp = 12,
        }

        impl TryFrom<u32> for Command {
            type Error = u32;
            fn try_from(this: u32) -> Result<Self, Self::Error> {
                Ok(match this {
                    0 => Self::FlashComplete,
                    1 => Self::FlashStatusUpdate,
                    2 => Self::BootloaderVersion,
                    3 => Self::BootloaderType,
                    4 => Self::GetBootloaderConfig,
                    5 => Self::BootloaderMemory,
                    6 => Self::BootApplication,
                    7 => Self::BootloaderCommit,
                    8 => Self::ReadFlash,
                    9 => Self::ApplicationDetails,
                    10 => Self::MemoryDetails,
                    11 => Self::IsUserBootloader,
                    12 => Self::NoOp,
                    o => return Err(o),
                })
            }
        }

        #[derive(Clone, Copy, Default, bytemuck::Zeroable, bytemuck::Pod)]
        #[repr(C)]
        pub struct BootloaderType {
            command: u32,
            ty: u32,
        }

        impl BootloaderType {
            pub fn ty(&self) -> Result<BootloaderTy, u32> {
                BootloaderTy::try_from(self.ty)
            }
        }

        #[derive(Debug)]
        pub enum BootloaderTy {
            Usb = 0,
            Network = 1,
        }

        impl TryFrom<u32> for BootloaderTy {
            type Error = u32;
            fn try_from(this: u32) -> Result<Self, Self::Error> {
                Ok(match this {
                    0 => Self::Usb,
                    1 => Self::Network,
                    o => return Err(o),
                })
            }
        }

        impl core::fmt::Debug for BootloaderType {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                let cmd = Command::try_from(self.command);
                let ty = BootloaderTy::try_from(self.ty);
                f.debug_struct("BootloaderType")
                    .field("command", &cmd)
                    .field("type", &ty)
                    .finish()
            }
        }
    }
}

#[derive(Debug)]
enum IoEvent {
    Pong,
    CreatedStream(StreamName, ReadStreamInfo, WriteStreamInfo),
    DeviceRead(u32, Vec<u8>),
    Shutdown,
}

enum DeviceEvent {
    CreateStream(EventHeader),
    NormalEvent(EventHeader),
    WriteEvent(EventHeader, Vec<u8>),
    // stream id, watchdog interval
    CreateWatchDog(u32, std::time::Duration),
    BulkWriteEvent {
        data: Vec<u8>,
        stream_id: u32,
        name: StreamName,
        split_by: usize,
    },
    ReadRelease {
        stream_id: u32,
        size: u32,
    }
}


async fn io_thread(mut stream: tokio::net::TcpStream, io_evs: mpsc::Sender<IoEvent>, mut device_evs: mpsc::Receiver<DeviceEvent>) {

    let mut header_buf = [0; 128];

    use tokio::io::AsyncReadExt;

    let mut requested_streams = HashMap::new();
    let mut device_requested_streams = HashMap::new();

    use tokio::io::AsyncWriteExt;

    let mut id = 0;

    let mut watchdog = None::<(u32, tokio::time::Interval)>;

    //let mut pending_chunks = HashMap::<u32, Chunks<u8>>::new();
    let mut pending_chunks = Vec::<(u32, Chunks<u8>)>::new();

    loop {
        if let Some(err) = stream.take_error().unwrap() {
            panic!("{err:?}");
        }

        let mut watchdog_task = async {
            if let Some((stream, interval)) = watchdog.as_mut() {
                interval.tick().await;
                *stream
            } else {
                std::future::pending::<()>().await;
                0
            }
        };

        const HEADER_LEN: usize = core::mem::size_of::<EventHeader>();
        let mut readable = (&mut stream).take(HEADER_LEN as _);

        tokio::select!{
            biased;

            stream_id = watchdog_task => {
                println!("sending wd");
                let data = [0, 0, 0, 0];
                let mut ev = Event::write(stream_id, b"", &data);
                ev.header.id = id;

                {
                    let header_buf = bytemuck::bytes_of(&ev.header);
                    stream.write(header_buf).await.unwrap();
                }
                stream.write(&data).await.unwrap();
                stream.flush().await.unwrap();
                id += 1;
            }
            ev = device_evs.recv() => {
                let Some(ev) = ev else {
                    continue;
                };

                match ev {
                    DeviceEvent::NormalEvent(mut ev) => {
                        ev.id = id;
                        {
                            let header_buf = bytemuck::bytes_of(&ev);
                            stream.write(header_buf).await.unwrap();
                            stream.flush().await.unwrap();
                        }
                        id += 1;
                    }
                    DeviceEvent::WriteEvent(mut ev, data) => {
                        ev.id = id;
                        {
                            let header_buf = bytemuck::bytes_of(&ev);
                            stream.write(header_buf).await.unwrap();
                        }
                        stream.write(&data).await.unwrap();
                        stream.flush().await.unwrap();
                        id += 1;
                    }
                    DeviceEvent::CreateStream(mut ev) => {
                        requested_streams.insert(ev.name, (WriteStreamInfo { write_size: ev.size, id: ev.stream_id }, false));

                        ev.id = id;
                        {
                            let header_buf = bytemuck::bytes_of(&ev);
                            stream.write(header_buf).await.unwrap();
                            stream.flush().await.unwrap();
                        }
                        id += 1;
                    }
                    DeviceEvent::CreateWatchDog(stream_id, period) => {
                        watchdog = Some((stream_id, tokio::time::interval(period)));
                    }
                    DeviceEvent::BulkWriteEvent {
                        data,
                        stream_id,
                        split_by,
                        name,
                    } => {
                        println!("inserting pending");
                        pending_chunks.push((stream_id, Chunks::new(data, split_by)));
                        //pending_chunks.insert(stream_id, );

                        /*
                        for bytes in data.chunks(split_by) {
                            let mut ev = Event::write(stream_id, &name, bytes);
                            println!("sending: {}", bytes.len());
                            ev.header.id = id;
                            {
                                let header_buf = bytemuck::bytes_of(&ev.header);
                                stream.write(header_buf).await.unwrap();
                            }
                            stream.write(bytes).await.unwrap();
                            id += 1;
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await
                        }
                        */
                    }
                    DeviceEvent::ReadRelease {
                        stream_id,
                        size,
                    } => {
                        let mut ev = Event::read_release(stream_id, b"", size);
                        ev.header.id = id;
                        {
                            let header_buf = bytemuck::bytes_of(&ev.header);
                            stream.write(header_buf).await.unwrap();
                            stream.flush().await.unwrap();
                        }
                        id += 1;
                    }
                }
            }
            res = readable.read(&mut header_buf) => {
                let len = match res {
                    Ok(len) => len,
                    Err(e) => {
                        if matches!(e.kind(), std::io::ErrorKind::ConnectionReset) {
                            io_evs.send(IoEvent::Shutdown).await.unwrap();
                            return;
                        } else {
                            panic!("{e:?}");
                        }
                    }
                };

                if len == 0 {
                    io_evs.send(IoEvent::Shutdown).await.unwrap();
                    return;
                } else if len < HEADER_LEN {
                    panic!("too small");
                }

                let (header_bytes, extra_bytes) = header_buf.split_at(HEADER_LEN);

                let header = bytemuck::from_bytes::<EventHeader>(header_bytes);

                let Ok(ty) = EventType::try_from(header.ty) else {
                    panic!("unknown event ty");
                };

                println!("received: {header:?} ({len:?})");

                match ty {
                    EventType::CreateStreamReq => {
                        let ev = Event::acknowledge_stream(header.stream_id, &header.name, header.size, header.id);

                        {
                            let header_buf = bytemuck::bytes_of(&ev.header);
                            stream.write(header_buf).await.unwrap();
                            stream.flush().await.unwrap();
                        }

                        fn requested_stream_finished(requested_streams: &mut HashMap<StreamName, (WriteStreamInfo, bool)> , name: &StreamName) -> Option<WriteStreamInfo> {
                            let (info, acked) = requested_streams.get(name)?;

                            if *acked {
                                requested_streams.remove(name).map(|(info, _)| info)
                            } else {
                                None
                            }
                        }

                        if let Some(existing) = requested_stream_finished(&mut requested_streams, &header.name) {
                            io_evs.send(IoEvent::CreatedStream(header.name, ReadStreamInfo { read_size: header.size, id: header.stream_id}, existing)).await.unwrap();
                        } else {
                            device_requested_streams.insert(header.name, ReadStreamInfo {
                                read_size: header.size,
                                id: header.stream_id,
                            });
                        }
                    }
                    EventType::CreateStreamResp => {
                        if let Some(existing) = device_requested_streams.remove(&header.name) {
                            let (host_requested, _) = requested_streams.remove(&header.name).unwrap();

                            io_evs.send(IoEvent::CreatedStream(header.name, existing, host_requested)).await.unwrap();
                        } else {
                            let (_, acked) = requested_streams.get_mut(&header.name).unwrap();
                            *acked = true;
                        }
                    }
                    EventType::ReadRelReq => {
                        let ev = Event::acknowledge_read_release(header.stream_id, &header.name, header.size, header.id);

                        {
                            let header_buf = bytemuck::bytes_of(&ev.header);
                            stream.write(header_buf).await.unwrap();
                            stream.flush().await.unwrap();
                        }

                        //tokio::time::sleep(std::time::Duration::from_millis(100)).await;

                        if let Some((stream_id, chunks)) = pending_chunks.last_mut() {

                            println!("got chunk");

                            let Some(pending) = chunks.next() else {
                                pending_chunks.pop();
                                continue;
                            };

                            let mut ev = Event::write(*stream_id, b"", &pending);
                            println!("sending: {}", pending.len());

                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            println!("sending");

                            ev.header.id = id;
                            {
                                let header_buf = bytemuck::bytes_of(&ev.header);
                                stream.write(header_buf).await.unwrap();
                            }
                            stream.write(&pending).await.unwrap();
                            stream.flush().await.unwrap();
                            id += 1;
                        }
                    }
                    EventType::PingResp => io_evs.send(IoEvent::Pong).await.unwrap(),
                    EventType::WriteReq => {
                        let mut read_buf = Vec::new();
                        let mut readable = (&mut stream).take(header.size as _);

                        let mut count = 0;
                        loop {
                            count += readable.read_buf(&mut read_buf).await.unwrap();

                            if count as u32 == header.size {
                                break;
                            }
                        }

                        io_evs.send(IoEvent::DeviceRead(header.stream_id, read_buf)).await.unwrap();

                        let ev = Event::acknowledge_write(header.stream_id, &header.name, header.size, header.id);

                        {
                            let header_buf = bytemuck::bytes_of(&ev.header);
                            stream.write(header_buf).await.unwrap();
                            stream.flush().await.unwrap();
                        }
                    }
                    EventType::WriteResp => {
                        println!("write resp received");
                        if let Some((stream_id, chunks)) = pending_chunks.last_mut() {

                            /*
                            println!("got chunk");

                            let Some(pending) = chunks.next() else {
                                pending_chunks.pop();
                                continue;
                            };

                            let mut ev = Event::write(*stream_id, b"", &pending);
                            println!("sending: {}", pending.len());

                            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                            println!("sending");

                            ev.header.id = id;
                            {
                                let header_buf = bytemuck::bytes_of(&ev.header);
                                stream.write(header_buf).await.unwrap();
                            }
                            stream.write(&pending).await.unwrap();
                            stream.flush().await.unwrap();
                            id += 1;
                            */
                        }
                    }
                    EventType::ReadResp | EventType::ReadRelResp | EventType::ReadRelSpecResp | EventType::CloseStreamResp => continue,

                    t => println!("skipping event ty {t:?}"),
                }
            }
        }
    }
}

struct Chunks<T> {
    iter: Vec<T>,
    split_by: usize,
    offset: usize,
}

impl <T> Chunks<T> {
    fn new(iter: Vec<T>, split_by: usize) -> Self {
        Self {
            iter,
            split_by,
            offset: 0,
        }
    }

    fn next(&mut self) -> Option<&[T]> {
        if self.offset >= self.iter.len() {
            return None;
        }

        let end = core::cmp::min(self.offset + self.split_by, self.iter.len());

        let items = &self.iter[self.offset..end];
        self.offset += self.split_by;
        Some(items)
    }
}

#[test]
fn chunks() {
    let vec = vec![1, 2, 3, 4, 5];

    let mut chunks = Chunks::new(vec, 2);

    assert_eq!(chunks.next(), Some(&[1, 2][..]));
    assert_eq!(chunks.next(), Some(&[3, 4][..]));
    assert_eq!(chunks.next(), Some(&[5][..]));
    assert_eq!(chunks.next(), None);
}
