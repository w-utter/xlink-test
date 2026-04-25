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

        const BOOTLOADER_STREAM_BYTES: &[u8] = b"__bootloader";
        const BOOTLOADER_STREAM: &str = "__bootloader";


        connection.create_stream(BOOTLOADER_STREAM, bootloader::MAX_PACKET_SIZE).await.unwrap();
        connection.create_stream("__watchdog", 64).await.unwrap();

        println!("waiting for streams");
        connection.wait_for_stream(BOOTLOADER_STREAM).await;
        connection.wait_for_stream("__watchdog").await;

        println!("got streams: {:?}", connection.created_streams);

        connection.create_watchdog(1, std::time::Duration::from_millis(2000)).await;

        //tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        // bootloader

        let bootloader_name = StreamName::new(&BOOTLOADER_STREAM_BYTES).unwrap();

        let bootloader = connection.created_streams.get_mut(&bootloader_name).unwrap();

        let bytes = {
            let data = bootloader::request::Command::GetBootloaderType as u32;
            let data_buf = bytemuck::bytes_of(&data).to_vec();
            let write = Event::write(0, &BOOTLOADER_STREAM_BYTES, &data_buf);

            bootloader.write(write, data_buf).await;
            bootloader.read().await
        };
        let bootloader_ty = bytemuck::from_bytes::<bootloader::response::BootloaderType>(&bytes);
        println!("bootloader: {bootloader_ty:?}");

        let Ok(ty) = bootloader_ty.ty() else {
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

            bootloader.write(write, data_buf).await;
            bootloader.bulk_write(device_firmware_buf.to_vec(), XLINK_MAX_PACKET_SIZE).await;

            /*
            connection.send_write_event(write, data_buf).await.unwrap();
            connection.send_bulk_write(device_firmware_buf.to_vec(), 0, StreamName::new(b"__bootloader").unwrap(), XLINK_MAX_PACKET_SIZE).await;
            */
        }

        connection.wait_for_shutdown().await;
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
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
        connection.create_stream("__rpc_main", bootloader::MAX_PACKET_SIZE).await.unwrap();
        connection.create_stream("__log", 128).await.unwrap();

        //TODO: might have to do smth for the timesync thread

        connection.create_stream("__timesync", 128).await.unwrap();

        // it would also prob be better to have channels to send stuff per each stream rather than
        // have it just be one way.
        // that way bulk writes can wait for an ack coming from the io
        // then reads can also be done per stream, & timeout for waiting for the write after the
        // read release can be done on this thread rather than in the io thread.
        //
        //
        // have one channel to go from this -> io
        //
        // have multiple channels to go back from io -> streams

        connection.wait_for_stream("__watchdog").await;
        connection.wait_for_stream("__rpc_main").await;
        connection.wait_for_stream("__log").await;


        println!("got streams: {:?}", connection.created_streams);

        connection.create_watchdog(0, std::time::Duration::from_millis(2000)).await;

        let rpc_name = StreamName::new(b"__rpc_main").unwrap();

        let mut rpc_stream = connection.created_streams.remove(&rpc_name).unwrap();

        // setup rpc
        // create a struct that borrows the stream




        
        let mut rpc = rpc::Rpc::new(&mut rpc_stream);

        let is_running = rpc.is_running().await;
        //let resp = rpc.call("isRunning", [].into_iter()).await;

        /*
        println!("\n\nrpc response: {is_running:?}");

        let res = rpc.enable_crash_dump(true).await;
        println!("enable crash dump: {res:?}");

        let mxid = rpc.mxid().await.unwrap();
        println!("mxid: {mxid:?}");

        let connected_cams = rpc.connected_cameras().await.unwrap();
        println!("cams: {connected_cams:?}");

        let connection_itfs = rpc.connection_interfaces().await.unwrap();
        println!("itfs: {connection_itfs:?}");


        let pairs = rpc.stereo_pairs().await.unwrap();
        println!("pairs: {pairs:?}");

        let names = rpc.camera_sensor_names().await.unwrap();
        println!("names: {names:?}");

        let imu = rpc.connected_imu().await.unwrap();
        println!("imu: {imu:?}");

        let ddr = rpc.ddr_memory_usage().await.unwrap();
        println!("ddr: {ddr:?}");

        let cmx = rpc.cmx_memory_usage().await.unwrap();
        println!("cmx: {cmx:?}");

        let css_heap = rpc.leon_css_heap_usage().await.unwrap();
        println!("css_heap: {css_heap:?}");

        let mss_heap = rpc.leon_mss_heap_usage().await.unwrap();
        println!("mss_heap: {mss_heap:?}");

        let temp = rpc.chip_temperature().await.unwrap();
        println!("temp: {temp:?}");

        let css_cpu = rpc.leon_css_cpu_usage().await.unwrap();
        println!("css_cpu: {css_cpu:?}");

        let mss_cpu = rpc.leon_mss_cpu_usage().await.unwrap();
        println!("mss_cpu: {mss_cpu:?}");

        /*
        let proc_mem = rpc.process_memory_usage().await.unwrap();
        println!("proc_mem: {proc_mem:?}");
        */

        let usb = rpc.usb_speed().await.unwrap();
        println!("usb speed: {usb:?}");

        let neural_depth = rpc.is_neural_depth_supported().await.unwrap();
        println!("neural depth support: {neural_depth:?}");

        let pipeline = rpc.is_pipeline_running().await.unwrap();
        println!("pipeline running: {pipeline:?}");

        let log_level = rpc.log_level().await.unwrap();
        println!("log level: {log_level:?}");

        let chunk_size = rpc.xlink_chunk_size().await.unwrap();
        println!("xlink chunk size: {chunk_size:?}");

        let ir_drivers = rpc.ir_drivers().await.unwrap();
        println!("ir drivers: {ir_drivers:?}");

        let crash_dump = rpc.crash_dump(false).await.unwrap();
        println!("crash dump: {crash_dump:?}");

        let has_crash_dump = rpc.has_crash_dump().await.unwrap();
        println!("has crash dump: {has_crash_dump:?}");

        let logging_rate = rpc.system_information_logging_rate().await.unwrap();
        println!("logging rate: {logging_rate:?}");

        let eeprom_available = rpc.is_eeprom_available().await.unwrap();
        println!("eeprom available: {eeprom_available:?}");

        let calibration = rpc.calibration().await.unwrap();
        println!("calibration: {calibration:?}");

        let calibration2 = rpc.calibration2().await.unwrap();
        println!("calibration2: {calibration2:?}");
        */

        let features = rpc.connected_camera_features().await.unwrap();
        println!("features: {features:?}");

        use crate::rpc::{PipelineSchema, NodeConnectionSchema, NodeObjInfo, NodeIoInfo, GlobalProperties, NodeType, LogLevel};

        const DEVICE_ID: &str = "19443010A1A1872D00";

        fn systeminfo_pipeline() -> (rpc::PipelineSchema, pipeline::OutputQueue<pipeline::SystemInfo, pipeline::RnopDeserializer, pipeline::queue_state::Pending>) {
            let mut pipe = pipeline::Pipeline::new();
            let mut log = pipe.create_node::<pipeline::SystemLogger>();

            let mut out = pipe.create_node::<pipeline::XLinkOut>();

            let xlink_out = pipe.create_output_queue(log.output(), &mut out);
            (pipe.build(DEVICE_ID), xlink_out)
        }

        fn imu_pipeline() -> (rpc::PipelineSchema, pipeline::OutputQueue<pipeline::ImuData, pipeline::RnopDeserializer, pipeline::queue_state::Pending>) {
            let mut pipe = pipeline::Pipeline::new();
            let mut imu = pipe.create_node::<pipeline::Imu>();


            imu.properties_mut().enable_sensor(pipeline::ImuSensorKind::Accelerometer, 400);
            imu.properties_mut().enable_sensor(pipeline::ImuSensorKind::GyroscopeCalibrated, 400);


            let mut out = pipe.create_node::<pipeline::XLinkOut>();

            let xlink_out = pipe.create_output_queue(imu.output(), &mut out);
            (pipe.build(DEVICE_ID), xlink_out)
        }

        fn camera_pipeline() -> (rpc::PipelineSchema, pipeline::OutputQueue<pipeline::CameraFrame, pipeline::RnopDeserializer, pipeline::queue_state::Pending>) {
            let mut pipe = pipeline::Pipeline::new();
            let mut camera = pipe.create_node::<pipeline::Camera>();

            let mut out = pipe.create_node::<pipeline::XLinkOut>();

            let xlink_out = pipe.create_output_queue(camera.raw_camera_output(), &mut out);
            (pipe.build(DEVICE_ID), xlink_out)
        }

        fn camera_pipeline_dynamic() -> (rpc::PipelineSchema, pipeline::OutputQueue<pipeline::CameraFrame, pipeline::RnopDeserializer, pipeline::queue_state::Pending>, pipeline::OutputQueue<pipeline::CameraFrame, pipeline::RnopDeserializer, pipeline::queue_state::Pending>) {
            let mut pipe = pipeline::Pipeline::new();
            let mut camera = pipe.create_node::<pipeline::Camera>();
            let mut out_1 = pipe.create_node::<pipeline::XLinkOut>();
            let mut out_2 = pipe.create_node::<pipeline::XLinkOut>();

            let id = camera.request_output(pipeline::CameraCapability {
                size: pipeline::Capability::new_single((1920, 1200)),
                fps: pipeline::Capability::new_none(),
                ty: None,
                enable_undistortion: None,
                isp_output: true,
                resize_mode: pipeline::FrameResize::Crop,
            });

            let output = camera.requested_camera_outputs().next().unwrap();

            let xlink_out1 = pipe.create_output_queue(output, &mut out_1);
            let xlink_out2 = pipe.create_output_queue(camera.raw_camera_output(), &mut out_2);
            (pipe.build(DEVICE_ID), xlink_out1, xlink_out2)
        }

        fn stereo_pipeline() -> (rpc::PipelineSchema, pipeline::OutputQueue<pipeline::CameraFrame, pipeline::RnopDeserializer, pipeline::queue_state::Pending>) {
            let mut pipe = pipeline::Pipeline::new();
            let mut camera_left = pipe.create_node::<pipeline::Camera>();
            let cam_l = camera_left.properties_mut();
            {
                cam_l.initial_control.af_region.x = 4909;
                cam_l.initial_control.af_region.priority = 4909;

                cam_l.initial_control.ae_lock_mode = true;
                cam_l.initial_control.awb_lock_mode = true;

                cam_l.initial_control.strobe_config.enable = true;
                cam_l.board_socket = crate::rpc::CameraBoardSocket::B;
            }

            camera_left.request_output(pipeline::CameraCapability {
                size: pipeline::Capability::new_single((1920, 1200)),
                fps: pipeline::Capability::new_none(),
                ty: None,
                enable_undistortion: None,
                isp_output: false,
                resize_mode: pipeline::FrameResize::Crop,
            });

            let cam_left = camera_left.requested_camera_outputs().next().unwrap();

            let mut camera_right = pipe.create_node::<pipeline::Camera>();
            let cam_r = camera_right.properties_mut();
            {
                cam_r.initial_control.ae_region.x = 3856;
                cam_r.initial_control.ae_region.y = 32228;
                cam_r.initial_control.ae_region.width = 23382;
                cam_r.initial_control.ae_region.height = 0;
                cam_r.initial_control.ae_region.priority = 3;

                cam_r.initial_control.af_region.width = 28518;
                cam_r.initial_control.af_region.height = 118;

                cam_r.initial_control.ae_lock_mode = true;
                cam_r.initial_control.awb_lock_mode = true;

                cam_r.initial_control.strobe_config.enable = true;
                cam_r.initial_control.contrast = -127;
                cam_r.initial_control.saturation = 2;
                cam_r.initial_control.low_power_frame_burst = 80;
                cam_r.initial_control.low_power_frame_discard = 84;
                cam_r.initial_control.enable_hdr = true;

                cam_r.board_socket = crate::rpc::CameraBoardSocket::C;
            }

            camera_right.request_output(pipeline::CameraCapability {
                size: pipeline::Capability::new_single((1920, 1200)),
                fps: pipeline::Capability::new_none(),
                ty: None,
                enable_undistortion: None,
                isp_output: true,
                resize_mode: pipeline::FrameResize::Crop,
            });

            let cam_right = camera_right.requested_camera_outputs().next().unwrap();


            let mut stereo = pipe.create_node::<pipeline::StereoDepth>();

            pipe.link(cam_left, stereo.input().left);
            pipe.link(cam_right, stereo.input().right);

            let mut out = pipe.create_node::<pipeline::XLinkOut>();

            let xlink_out = pipe.create_output_queue(stereo.output().disparity, &mut out);

            (pipe.build(DEVICE_ID), xlink_out)
        }

        let (schema, out) = stereo_pipeline();
        //let (schema, out1, out2) = camera_pipeline_dynamic();


        //connection.create_stream("__x_0_out", bootloader::MAX_PACKET_SIZE).await.unwrap();

        let ret = rpc.set_pipeline_schema(schema).await;
        println!("{ret:?}");

        let ret = rpc.wait_for_device_ready().await;
        println!("{ret:?}");

        let ret = rpc.build_pipeline().await;
        println!("{ret:?}");

        let ret = rpc.start_pipeline().await;
        println!("{ret:?}");

        /*
        let mut queue1 = connection.wait_for_output_queue(out1).await;
        let mut queue2 = connection.wait_for_output_queue(out2).await;
        */

        let mut queue = connection.wait_for_output_queue(out).await;


        loop {
            let r = queue.read().await;
            println!("{:?}", r.unwrap().0);
            /*
            tokio::select! {
                r = queue1.read() => {
                    //println!("received custom: {:?}", r.unwrap().0);
                }
                r = queue2.read() => {
                    //println!("received raw: {:?}", r.unwrap().0);
                }
            }
            */
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
    created_streams: HashMap<StreamName, ConnectionStream>,
    io_events: mpsc::Receiver<IoEvent>,
    device_events: mpsc::Sender<DeviceEvent>,


    next_stream_id: u32,
}

#[derive(Debug)]
struct ConnectionStream {
    read: ReadStreamInfo,
    write: WriteStreamInfo,
    writer: mpsc::Sender<StreamEvent>,
    writer_fb: Arc<Notify>,
    reader: mpsc::Receiver<Vec<u8>>,
}

enum StreamEvent {
    Write(EventHeader, Vec<u8>),
    ReadRelease(EventHeader),
}

use tokio::sync::Notify;
use std::sync::Arc;

impl ConnectionStream {
    fn new(read: ReadStreamInfo, write: WriteStreamInfo, writer: mpsc::Sender<StreamEvent>) -> (Self, Arc<Notify>, mpsc::Sender<Vec<u8>>) {
        const CHANNEL_SIZE: usize = 8;
        let (reader_tx, reader_rx) = mpsc::channel(CHANNEL_SIZE);

        let notify = Arc::new(Notify::new());

        (Self {
            read,
            write,
            writer,
            writer_fb: notify.clone(),
            reader: reader_rx,
        }, notify, reader_tx)
    }

    /*
    async fn send_write_event<T: bytemuck::Pod + bytemuck::Zeroable>(&mut self, ev: Event<T>, data: Vec<u8>) -> std::io::Result<()> {
        self.device_events.send(DeviceEvent::WriteEvent(ev.header, data)).await.unwrap();
            Ok(())
    }
    */
    async fn write<T: bytemuck::Pod + bytemuck::Zeroable>(&mut self, ev: Event<T>, data: Vec<u8>) {
        self.writer.send(StreamEvent::Write(ev.header, data)).await.unwrap();
        self.writer_fb.notified().await;
    }

    async fn bulk_write(&mut self, data: Vec<u8>, split_by: usize) {
        let mut chunks = Chunks::new(data, split_by);
        while let Some(bytes) = chunks.next() {
            let event = Event::write(self.write.id, b"", bytes);
            self.write(event, bytes.to_vec()).await;
        }
    }

    async fn read(&mut self) -> Vec<u8> {
        let bytes = self.reader.recv().await.unwrap();
        bytes
    }
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

    /*
    async fn send_write_event<T: bytemuck::Pod + bytemuck::Zeroable>(&mut self, ev: Event<T>, data: Vec<u8>) -> std::io::Result<()> {
        self.device_events.send(DeviceEvent::WriteEvent(ev.header, data)).await.unwrap();
            Ok(())
    }

    async fn send_bulk_write(&mut self, data: Vec<u8>, stream_id: u32, name: StreamName, split_by: usize) {
        self.device_events.send(DeviceEvent::BulkWriteEvent{ data, stream_id, name, split_by }).await.unwrap();
    }
    */

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
        self.create_stream_bytes(name_bytes, write_size).await
    }

    async fn create_stream_bytes(&mut self, name: &[u8], write_size: u32) -> std::io::Result<()> {
        let stream_id = self.next_stream_id;

        let ev = Event::create_stream(stream_id, &name, write_size);

        self.next_stream_id += 1;

        let stream_name = ev.header.name;

        self.device_events.send(DeviceEvent::CreateStream(ev.header)).await.unwrap();
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
        let name = StreamName::new(&name.as_bytes()).unwrap();

        if self.created_streams.get(&name).is_some() {
            return
        }

        loop {
            let Some(ev) = self.io_events.recv().await else {
                panic!()
            };

            match ev {
                IoEvent::CreatedStream(created_name, stream) => {
                    self.created_streams.insert(created_name, stream);
                    if created_name == name {
                        return;
                    }
                }
                o => panic!("{o:?}")
            }
        }
    }

    /*
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
    */

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

    async fn wait_for_output_queue<O, D>(&mut self, queue: pipeline::OutputQueue<O, D, pipeline::queue_state::Pending>) -> pipeline::OutputQueue<O, D, pipeline::queue_state::Ready> {
        let name = queue.state.0;

        if let Some(stream) = self.created_streams.remove(&name) {
            return pipeline::OutputQueue {
                state: pipeline::queue_state::Ready(stream),
                _pd: core::marker::PhantomData,
            };
        }

        self.create_stream_bytes(name.as_bytes(), bootloader::MAX_PACKET_SIZE).await.unwrap();

        loop {
            let Some(ev) = self.io_events.recv().await else {
                panic!()
            };

            match ev {
                IoEvent::CreatedStream(created_name, stream) => {
                    if created_name == name {
                        return pipeline::OutputQueue {
                            state: pipeline::queue_state::Ready(stream),
                            _pd: core::marker::PhantomData,
                        };
                    }
                    self.created_streams.insert(created_name, stream);
                }
                o => panic!("{o:?}")
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
    fn read_release<N: Borrow<[u8]>>(stream_id: u32, name: &N, len: u32, id: u32) -> Self {
        let mut ev = Event::new(ReadRelease, name.borrow(), EventType::ReadRelReq as u32, stream_id, 1);
        ev.header.size = len;
        ev.header.id = id;
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
    CreatedStream(StreamName, ConnectionStream),
    Shutdown,
}

enum DeviceEvent {
    CreateStream(EventHeader),
    NormalEvent(EventHeader),
    //WriteEvent(EventHeader, Vec<u8>),
    // stream id, watchdog interval
    CreateWatchDog(u32, std::time::Duration),
    /*
    BulkWriteEvent {
        data: Vec<u8>,
        stream_id: u32,
        name: StreamName,
        split_by: usize,
    },
    */
}


async fn io_thread(mut stream: tokio::net::TcpStream, io_evs: mpsc::Sender<IoEvent>, mut device_evs: mpsc::Receiver<DeviceEvent>) {

    let mut header_buf = [0; 128];
    let mut header_offset = 0;

    use tokio::io::AsyncReadExt;

    let mut requested_streams = HashMap::new();
    let mut device_requested_streams = HashMap::new();

    use tokio::io::AsyncWriteExt;

    let mut id = 0;

    let mut watchdog = None::<(u32, tokio::time::Interval)>;

    //let mut pending_chunks = HashMap::<u32, Chunks<u8>>::new();
    //let mut pending_chunks = Vec::<(u32, Chunks<u8>)>::new();

    const STREAM_SIZE: usize = 32;
    let (stream_tx, mut stream_rx) = mpsc::channel(STREAM_SIZE);


    struct IoStream {
        writer_fb: Arc<Notify>,
        reader: mpsc::Sender<Vec<u8>>,
    }

    impl IoStream {
        fn new( writer_fb: Arc<Notify>, reader: mpsc::Sender<Vec<u8>>,) -> Self {
            Self {
                writer_fb,
                reader,
            }
        }
    }

    let mut created_streams = HashMap::<u32, IoStream>::new();

    loop {
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
        let mut readable = (&mut stream).take((HEADER_LEN - header_offset) as _);

        tokio::select!{
            biased;

            stream_id = watchdog_task => {
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
                }
            }
            res = readable.read(&mut header_buf[header_offset..]) => {
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
                } else if header_offset + len < HEADER_LEN {
                    header_offset += len;
                    continue;
                }
                header_offset = 0;

                let (header_bytes, extra_bytes) = header_buf.split_at(HEADER_LEN);

                let header = bytemuck::from_bytes::<EventHeader>(header_bytes);

                let Ok(ty) = EventType::try_from(header.ty) else {
                    panic!("unknown event ty");
                };

                //println!("received: {header:?} ({len:?})");

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

                            let stream_id = existing.id;
                            let (conn_stream, writer_fb, reader) = ConnectionStream::new(ReadStreamInfo {read_size: header.size, id: header.stream_id}, existing, stream_tx.clone());

                            created_streams.insert(stream_id, IoStream::new(writer_fb, reader));
                            io_evs.send(IoEvent::CreatedStream(header.name, conn_stream)).await.unwrap();
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

                            let (conn_stream, writer_fb, reader) = ConnectionStream::new(existing, host_requested, stream_tx.clone());

                            created_streams.insert(header.stream_id, IoStream::new(writer_fb, reader));

                            io_evs.send(IoEvent::CreatedStream(header.name, conn_stream)).await.unwrap();
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

                        if let Some(IoStream { reader, ..}) = created_streams.get_mut(&header.stream_id) {
                            reader.send(read_buf).await.unwrap();


                            let ev = Event::acknowledge_write(header.stream_id, &header.name, header.size, header.id);

                            {
                                let header_buf = bytemuck::bytes_of(&ev.header);
                                stream.write(header_buf).await.unwrap();
                                stream.flush().await.unwrap();
                            }

                            let ev = Event::read_release(header.stream_id, &header.name, header.size, header.id);

                            {
                                let header_buf = bytemuck::bytes_of(&ev.header);
                                stream.write(header_buf).await.unwrap();
                                stream.flush().await.unwrap();
                            }
                        } else {
                            panic!("could not find stream for {}", header.stream_id);
                        }
                    }
                    EventType::WriteResp => {
                        if let Some(IoStream { writer_fb, .. }) = created_streams.get_mut(&header.stream_id) {
                            writer_fb.notify_one();
                        } else {
                            panic!("could not find stream for {}", header.stream_id);
                        }
                    }
                    EventType::ReadResp | EventType::ReadRelResp | EventType::ReadRelSpecResp | EventType::CloseStreamResp => continue,

                    t => println!("skipping event ty {t:?}"),
                }
            }
            ev = stream_rx.recv() => {
                let Some(ev) = ev else {
                    continue;
                };

                match ev {
                    StreamEvent::Write(mut header, data) => {
                        println!("sending: {}", data.len());
                        header.id = id;
                        {
                            let header_buf = bytemuck::bytes_of(&header);
                            stream.write(header_buf).await.unwrap();
                        }
                        stream.write(&data).await.unwrap();
                        stream.flush().await.unwrap();
                        id += 1;
                    }
                    StreamEvent::ReadRelease(mut header) => {
                        header.id = id;
                        {
                            let header_buf = bytemuck::bytes_of(&header);
                            stream.write(header_buf).await.unwrap();
                        }
                        stream.flush().await.unwrap();
                        id += 1;
                    }
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

mod pipeline {
    use std::collections::HashMap;
    use crate::rpc::{LogLevel, NodeType};


    // this is used 'per node' related to the nodes operation
    // => it will be there in prob every device io_info
    /*
    fn pipeline_event_output(id: u32) -> NodeIoInfo {
        NodeIoInfo {
            blocking: false,
            group: "".into(),
            id,
            name: "pipelineEventOutput".into(),
            queue_size: 8,
            ty: NodeType::MSender,
            wait_for_message: false,
        }
    }
    */

    fn register_node<'a, N: Node>(n: &'a NodeT<N>, map: &mut HashMap<u32, InternalNodeInfo<'a>>, io_idx: &mut u32) {
        let mut w = vec![];

        n.properties.serialize(&mut w).unwrap();

        let mut io_info = IoInfo::new(io_idx);

        n.input.register(&mut io_info);
        n.output.register(&mut io_info);

        let info = InternalNodeInfo {
            name: N::NAME,
            alias: N::ALIAS,
            properties: w,
            log_level: n.log_level,
            io_info: io_info.inner,
        };

        map.insert(n.id, info);
    }

    #[derive(Debug)]
    struct InternalNodeInfo<'a> {
        name: &'a str,
        alias: Option<&'a str>,
        properties: Vec<u8>,
        log_level: LogLevel,
        io_info: HashMap<(Option<&'a str>, &'a str), InternalIoInfo<'a>>
    }

    pub trait Deserializer {
        type Error;
        fn deserialize<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Self::Error>;
    }

    pub(crate) trait Deserialize<D: Deserializer>: Sized + serde::de::DeserializeOwned {
        fn deserialize(bytes: &[u8]) -> Result<Self, D::Error> {
            D::deserialize(bytes)
        }
    }

    trait NotAnySerializer {}

    pub struct ProtobufDeserializer;
    pub struct ProtobufSerializer;
    pub struct RnopDeserializer;
    pub struct RnopSerializer;
    pub struct AnySerializer;
    pub struct AnyDeserializer;

    macro_rules! not_any_serializer {
        ($($ser:ty),*) => {
            $(
                impl NotAnySerializer for $ser {}
            )*
        }
    }

    not_any_serializer!(ProtobufDeserializer, ProtobufSerializer, RnopDeserializer, RnopSerializer, MsgpackDeserializer, MsgpackSerializer);

    impl Serializer for AnySerializer {
        type Error = ();
        fn serialize<T: serde::Serialize, W: std::io::Write>(t: &T, writer: &mut W) -> Result<usize, Self::Error> {
            todo!()
        }
    }


    impl Serializer for RnopSerializer {
        type Error = ();
        fn serialize<T: serde::Serialize, W: std::io::Write>(t: &T, writer: &mut W) -> Result<usize, Self::Error> {
            let value = rnop::to_value(t).unwrap();
            let size = value.write(writer).unwrap();
            Ok(size)
        }
    }

    impl Deserializer for RnopDeserializer {
        type Error = ();
        fn deserialize<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Self::Error> {
            let value = rnop::Value::parse(bytes).unwrap();
            // FIXME: for some reason deserializing single field structs does not work, issue with the lib
            Ok(rnop::from_value::<T>(value).unwrap())
        }
    }

    struct MsgpackDeserializer;
    struct MsgpackSerializer;

    impl Serializer for MsgpackSerializer {
        type Error = ();
        fn serialize<T: serde::Serialize, W: std::io::Write>(t: &T, writer: &mut W) -> Result<usize, Self::Error> {
            todo!()
        }
    }

    pub trait Serializer {
        type Error;
        fn serialize<T: serde::Serialize, W: std::io::Write>(t: &T, writer: &mut W) -> Result<usize, Self::Error>;
    }

    trait Serialize<S: Serializer>: Sized + serde::Serialize {
        fn serialize<W: std::io::Write>(&self, writer: &mut W) -> Result<usize, S::Error> {
            S::serialize(self, writer)
        }
    }

    impl <T: serde::Serialize, S: Serializer> Serialize<S> for T {}
    impl <T: serde::de::DeserializeOwned, D: Deserializer> Deserialize<D> for T {}

    pub trait Node {
        type Input: IoRegister + Inputs + Default;
        type Output: IoRegister + Outputs + Default;

        type Properties: Default + Serialize<RnopSerializer>;
        const NAME: &str;
        const ALIAS: Option<&str> = None;
    }

    //TODO: having a generic way to have &mut refs to nodes with and without debug

    struct Debug<T> {
        inner: T,
    }

    #[derive(Default)]
    struct PipelineEventOutput {
        inner: Output<PipelineEvent, RnopDeserializer>,
    }

    impl <D: Deserializer> IoDeserializeable<D> for PipelineEvent {
        type Metadata = PipelineEvent;
        type Output = ();
    }

    #[derive(serde::Deserialize)]
    struct PipelineEvent {

    }

    impl NotAny for PipelineEvent {}

    impl StaticIoDesc for PipelineEvent {
        //TODO
        const NAME: &str = "";
        const NODE_TYPE: NodeType = NodeType::MSender;
    }

    impl StaticIoDesc for PipelineEventOutput {
        const NAME: &str = PipelineEvent::NAME;
        const NODE_TYPE: NodeType = PipelineEvent::NODE_TYPE;
        const GROUP: Option<&str> = <PipelineEvent as StaticIoDesc>::GROUP;
    }

    impl IoRegister for PipelineEventOutput {
        fn register<'a, 'b>(&'a self, info: &mut IoInfo<'a, 'b>) {
            todo!()
        }
    }

    impl <O: Outputs> Outputs for (PipelineEventOutput, O) {
        type Outputs<'a, N> = O::Outputs<'a, N> where Self: 'a, N: Node + 'a;
        fn outputs<'a, T: Node>(&'a self, node: &'a NodeT<T>) -> Self::Outputs<'a, T> {
            self.1.outputs(node)
        }
    }

    impl <I1: Inputs, I2: Inputs> Inputs for (I1, I2) {
        type Inputs<'a, N> = (I1::Inputs<'a, N>, I2::Inputs<'a, N>) where Self: 'a, N: Node + 'a;
        fn inputs<'a, T: Node>(&'a self, node: &'a NodeT<T>) -> Self::Inputs<'a, T> {
            (self.0.inputs(node), self.1.inputs(node))
        }

    }

    impl <O1: Outputs, O2: Outputs> Outputs for (O1, O2) {
        type Outputs<'a, N> = (O1::Outputs<'a, N>, O2::Outputs<'a, N>) where Self: 'a, N: Node + 'a;
        fn outputs<'a, T: Node>(&'a self, node: &'a NodeT<T>) -> Self::Outputs<'a, T> {
            (self.0.outputs(node), self.1.outputs(node))
        }
    }

    impl <R1: IoRegister, R2: IoRegister> IoRegister for (R1, R2) {
        fn register<'a, 'b>(&'a self, info: &mut IoInfo<'a, 'b>) {
            self.0.register(info);
            self.1.register(info);
        }
    }

    impl <T: Node> Node for Debug<T> {
        type Input = T::Input;
        type Output = (PipelineEventOutput, T::Output);
        type Properties = T::Properties;
        const NAME: &str = T::NAME;
        const ALIAS: Option<&str> = T::ALIAS;
    }

    impl <T: Node> NodeT<Debug<T>> {
        fn debug_out(&self) -> OutputRef<'_, T, PipelineEvent, RnopDeserializer> {
            todo!()
        }
    }

    // for registering to the pipeline
    struct IoInfo<'a, 'b> {
        current_id: &'b mut u32,
        inner: HashMap<(Option<&'a str>, &'a str), InternalIoInfo<'a>>
    }

    #[derive(Debug)]
    struct InternalIoInfo<'a> {
        ty: NodeType,
        conf: &'a IoDescConf,
        id: u32,
    }

    impl <'a, 'b> IoInfo<'a, 'b> {
        fn new(id: &'b mut u32) -> Self {
            Self {
                current_id: id,
                inner: Default::default(),
            }
        }

        pub fn push(&mut self, group: Option<&'a str>, name: &'a str, ty: NodeType, conf: &'a IoDescConf) {
            let info = InternalIoInfo {
                ty,
                id: *self.current_id,
                conf,
            };

            *self.current_id += 1;

            self.inner.insert((group, name), info);
        }
    }

    trait IoRegister {
        fn register<'a, 'b>(&'a self, info: &mut IoInfo<'a, 'b>);
    }

    pub trait Inputs {
        type Inputs<'a, N> where Self: 'a, N: Node + 'a;
        fn inputs<'a, T: Node>(&'a self, node: &'a NodeT<T>) -> Self::Inputs<'a, T>;
    }

    pub trait Outputs {
        type Outputs<'a, N> where Self: 'a, N: Node + 'a;
        fn outputs<'a, T: Node>(&'a self, node: &'a NodeT<T>) -> Self::Outputs<'a, T>;
    }

    trait IoDesc {
        fn name(this: &Self::InfoStorage) -> &str;

        const GROUP: Option<&str> = None;

        fn node_type(this: &Self::InfoStorage) -> NodeType;

        type InfoStorage: Default;
    }

    trait StaticIoDesc {
        const NAME: &str;
        const GROUP: Option<&str> = None;
        const NODE_TYPE: NodeType;
    }

    impl <T: StaticIoDesc> IoDesc for T {
        fn name(_: &Self::InfoStorage) -> &str {
            T::NAME
        }

        const GROUP: Option<&str> = T::GROUP;

        fn node_type(_: &Self::InfoStorage) -> NodeType {
            T::NODE_TYPE
        }

        type InfoStorage = ();
    }

    pub struct XLI;
    #[derive(Default)]
    pub struct Empty;

    impl IoRegister for Empty {
        fn register(&self, info: &mut IoInfo<'_, '_>) {
        }
    }

    impl Inputs for Empty {
        type Inputs<'a, N: Node + 'a> = ();
        fn inputs<'a, T: Node>(&'a self, node: &'a NodeT<T>) -> Self::Inputs<'a, T> {}
    }
    impl Outputs for Empty {
        type Outputs<'a, N: Node + 'a> = ();
        fn outputs<'a, T: Node>(&'a self, node: &'a NodeT<T>) -> Self::Outputs<'a, T> {}
    }

    impl <T: IoDesc + IoSerializeable<S>, S: Serializer> Inputs for Input<T, S> {
        type Inputs<'a, N: Node + 'a> = InputRef<'a, N, T, S> where Self: 'a;
        fn inputs<'a, N: Node>(&'a self, node: &'a NodeT<N>) -> Self::Inputs<'a, N> {
            InputRef {
                node,
                input: self,
            }
        }
    }

    impl StaticIoDesc for XLI {
        const NAME: &str = "in";
        const NODE_TYPE: NodeType = NodeType::SReceiver;
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    pub struct XLinkOutProperties {
        pub max_fps_limit: f32,
        pub(crate) stream_name: String,
        pub metadata_only: bool,
        pub packet_size: i32,
        pub bytes_per_second: i32,
    }

    impl core::default::Default for XLinkOutProperties {
        fn default() -> Self {
            Self {
                max_fps_limit: -1.,
                stream_name: Default::default(),
                metadata_only: false,
                packet_size: -1,
                bytes_per_second: -1,
            }
        }
    }

    #[derive(Clone, Copy)]
    pub struct InputRef<'a, P: Node, T: IoDesc + IoSerializeable<S>, S: Serializer> {
        node: &'a NodeT<P>,
        input: &'a Input<T, S>,
    }

    #[derive(Clone, Copy)]
    pub struct OutputRef<'a, P: Node, T: IoDesc + IoDeserializeable<D>, D: Deserializer> {
        node: &'a NodeT<P>,
        output: &'a Output<T, D>,
    }

    pub struct Input<T: IoDesc + IoSerializeable<S>, S: Serializer> {
        input: T::InfoStorage,
        conf: IoDescConf,
        _pd: core::marker::PhantomData<S>,
    }

    impl <T: IoDesc + IoSerializeable<S>, S: Serializer> core::default::Default for Input<T, S> {
        fn default() -> Self {
            Self {
                input: Default::default(),
                conf: Default::default(),
                _pd: core::marker::PhantomData,
            }
        }
    }

    impl <S: Serializer, I> IoSerializeable<S> for Any<I> {
        type Metadata = ();
        type Output = Vec<u8>;
    }

    impl Node for XLinkOut {
        type Input = Input<Any<XLI>, AnySerializer>;
        type Output = Empty;
        type Properties = XLinkOutProperties;
        const NAME: &str = "XLinkOut";
    }

    pub struct XLinkOut;
    pub struct SystemLogger;

    struct DynamicDesc {
        name: String,
        node_type: NodeType,
    }

    pub struct Output<T: IoDesc + IoDeserializeable<D>, D: Deserializer> {
        output: T::InfoStorage,
        conf: IoDescConf,
        _pd: core::marker::PhantomData<D>,
    }

    #[repr(transparent)]
    pub struct Dynamic<T> {
        _inner: core::marker::PhantomData<T>,
    }

    impl <D: Deserializer, T: IoDeserializeable<D>> IoDeserializeable<D> for Dynamic<T> {
        type Metadata = T::Metadata;
        type Output = T::Output;
    }

    #[derive(Default)]
    struct DynamicStorage {
        name: String,
    }

    trait DynamicGroup {
        const GROUP: Option<&str>;
    }

    impl <T: StaticIoDesc + DynamicGroup> IoDesc for Dynamic<T> {
        fn name(this: &Self::InfoStorage) -> &str {
            this.name.as_str()
        }

        const GROUP: Option<&str> = <T as DynamicGroup>::GROUP;
        fn node_type(_: &Self::InfoStorage) -> NodeType {
            T::NODE_TYPE
        }

        type InfoStorage = DynamicStorage;
    }

    // these need to move to sealed types if actually using them
    impl <T: IoDesc + IoDeserializeable<D>, D: Deserializer> core::default::Default for Output<T, D> {
        fn default() -> Self {
            Self {
                output: Default::default(),
                conf: Default::default(),
                _pd: core::marker::PhantomData,
            }
        }
    }

    trait CompatibleLink {}
    trait CompatibleSerialization {}

    // the unit type here is repr of Any
    impl <T: NotAny> CompatibleLink for (T, T) {}
    impl <T: NotAny> CompatibleLink for ((), T) {}
    impl <T: NotAny> CompatibleLink for (T, ()) {}
    impl CompatibleLink for ((), ()) {}

    pub struct Any<I> {
        inner: I,
    }

    impl <I> serde::Serialize for Any<I> {
        fn serialize<S: serde::ser::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            todo!()
        }
    }

    impl <T: NotAnySerializer> CompatibleSerialization for (AnySerializer, T) {}
    impl <T: NotAnySerializer> CompatibleSerialization for (T, AnyDeserializer) {}
    impl CompatibleSerialization for (AnySerializer, AnyDeserializer) {}
    impl CompatibleSerialization for (RnopSerializer, RnopDeserializer) {}
    impl CompatibleSerialization for (MsgpackSerializer, MsgpackDeserializer) {}
    impl CompatibleSerialization for (ProtobufSerializer, ProtobufDeserializer) {}

    pub mod queue_state {
        pub struct Pending(pub(crate) crate::StreamName);
        pub struct Ready(pub(crate) crate::ConnectionStream);
    }

    pub struct OutputQueue<T, D, S> {
        pub(crate) state: S,
        pub(crate) _pd: core::marker::PhantomData<(T, D)>,
    }

    // used as a marker for link support
    trait NotAny {}

    pub trait IoDeserializeable<D: Deserializer> {
        type Metadata: Deserialize<D>;
        type Output;
    }

    pub trait IoSerializeable<S: Serializer> {
        type Metadata: Serialize<S>;
        type Output;
    }

    pub trait Simplify<D: Deserializer, T: IoDeserializeable<D>> {
        type Out;
        fn simplify(this: <T as IoDeserializeable<D>>::Metadata, bytes: Vec<u8>) -> Self::Out;
    }

    impl <T: IoDeserializeable<D>, D: Deserializer> Simplify<D, T> for (<T as IoDeserializeable<D>>::Metadata, ()) {
        type Out = <T as IoDeserializeable<D>>::Metadata;

        fn simplify(this: <T as IoDeserializeable<D>>::Metadata, _: Vec<u8>) -> Self::Out {
            this
        }
    }

    impl <T: IoDeserializeable<D>, D: Deserializer> Simplify<D, T> for (<T as IoDeserializeable<D>>::Metadata, Vec<u8>) {
        type Out = (<T as IoDeserializeable<D>>::Metadata, Vec<u8>);

        fn simplify(this: <T as IoDeserializeable<D>>::Metadata, bytes: Vec<u8>) -> Self::Out {
            (this, bytes)
        }
    }

    impl <T: IoDeserializeable<D>, D: Deserializer> OutputQueue<T, D, queue_state::Ready> {
        /// this is a whole lot of trait magic to be able to either output `Metadata` or `(Metadata, Vec<u8>)` depending on the type
        pub async fn read(&mut self) -> Result<<(T::Metadata, T::Output) as Simplify<D, T>>::Out, D::Error> where (<T as IoDeserializeable<D>>::Metadata, <T as IoDeserializeable<D>>::Output): Simplify<D, T> {
            let ReadRaw { metadata, buffer } = self.read_raw().await.unwrap();

            let metadata = D::deserialize::<T::Metadata>(&metadata)?;

            let out = <(<T as IoDeserializeable<D>>::Metadata, <T as IoDeserializeable<D>>::Output)>::simplify(metadata, buffer);

            Ok(out)
        }
    }

    #[derive(Debug)]
    pub struct ReadRaw {
        pub metadata: Vec<u8>,
        pub buffer: Vec<u8>,
    }

    impl <T, D> OutputQueue<T, D, queue_state::Ready> {
        pub async fn read_raw(&mut self) -> Option<ReadRaw> {
            let mut bytes = self.state.0.read().await;

            struct Header {
                metadata_size: u32,
                ty: u32,
            }

            impl Header {
                const SIZE: usize = 16 + 4 + 4;
                const END_OF_PACKET_MARKER: [u8; 16] = [0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0];

                fn parse(bytes: &[u8]) -> Result<Self, ()> {
                    if bytes.len() < 24 {
                        return Err(());
                    }

                    let chunk = bytes.last_chunk::<16>().unwrap();
                    if *chunk != Self::END_OF_PACKET_MARKER {
                        return Err(())
                    }

                    let take_u32 = |bytes: &[u8]| -> u32 {
                        u32::from_le_bytes(*bytes.last_chunk::<4>().unwrap())
                    };

                    let packet_len = bytes.len() - Self::END_OF_PACKET_MARKER.len();
                    let metadata_size = take_u32(&bytes[..packet_len]);
                    let ty = take_u32(&bytes[..packet_len - 4]);

                    Ok(Self {
                        metadata_size,
                        ty,
                    })
                }
            }

            let header = Header::parse(&bytes).ok()?;

            bytes.truncate(bytes.len() - Header::SIZE);

            let metadata_size = header.metadata_size as usize;

            if bytes.len() < metadata_size {
                return None;
            }

            let metadata = bytes.split_off(bytes.len() - metadata_size);

            Some(ReadRaw {
                metadata,
                buffer: bytes,
            })
        }
    }

    impl <T: StaticIoDesc + IoDeserializeable<D>, D: Deserializer> StaticIoDesc for Output<T, D> {
        const NAME: &str = T::NAME;
        const GROUP: Option<&str> = T::GROUP;
        const NODE_TYPE: NodeType = T::NODE_TYPE;
    }

    impl <T: StaticIoDesc + IoSerializeable<S>, S: Serializer> StaticIoDesc for Input<T, S> where T: Serialize<S> {
        const NAME: &str = T::NAME;
        const GROUP: Option<&str> = T::GROUP;
        const NODE_TYPE: NodeType = T::NODE_TYPE;
    }

    impl <T: IoDesc> IoDesc for Any<T> {
        fn name(this: &Self::InfoStorage) -> &str {
            T::name(this)
        }

        const GROUP: Option<&str> = T::GROUP;

        fn node_type(this: &Self::InfoStorage) -> NodeType {
            T::node_type(this)
        }

        type InfoStorage = T::InfoStorage;
    }

    impl StaticIoDesc for SystemInfo {
        const NAME: &str = "out";
        const NODE_TYPE: NodeType = NodeType::MSender;
    }
    
    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    pub struct SystemLoggerProperties {
        pub rate_hz: f32,
    }

    impl core::default::Default for SystemLoggerProperties {
        fn default() -> Self {
            Self {
                rate_hz: 1.,
            }
        }
    }

    impl Node for SystemLogger {
        type Input = Empty;
        type Output = Output<SystemInfo, RnopDeserializer>;
        type Properties = SystemLoggerProperties;
        const NAME: &str = "SystemLogger";
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    pub struct ImuProperties {
        imu_sensors: Vec<ImuSensorConfig>,
        batch_report_threshold: i32,
        max_batch_reports: i32,
        enable_firmware_update: Option<bool>,
    }

    impl ImuProperties {
        pub fn enable_sensor(&mut self, sensor: ImuSensorKind, rate: u32) {
            self.imu_sensors.push(ImuSensorConfig {
                sensor_id: sensor,
                report_rate: rate,
                ..Default::default()
            })
        }
    }

    impl core::default::Default for ImuProperties {
        fn default() -> Self {
            Self {
                imu_sensors: vec![],
                batch_report_threshold: 1,
                max_batch_reports: 5,
                enable_firmware_update: Some(false),
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug)]
    pub struct ImuSensorConfig {
        sensitivity_enabled: bool,
        sensitivity_relative: bool,
        change_sensitivity: u16,
        report_rate: u32,
        sensor_id: ImuSensorKind,
    }

    impl core::default::Default for ImuSensorConfig {
        fn default() -> Self {
            Self {
                sensitivity_enabled: false,
                sensitivity_relative: false,
                change_sensitivity: 0,
                report_rate: 100,
                sensor_id: ImuSensorKind::Accelerometer,
            }
        }
    }

    #[derive(serde_repr::Serialize_repr, serde_repr::Deserialize_repr, Debug)]
    #[repr(i32)]
    pub enum ImuSensorKind {
        /// raw acceleration without preprocessing
        AccelerometerRaw = 0x14,
        /// acceleration including gravity
        Accelerometer = 0x01,
        /// acceleration without gravity
        LinearAcceleration = 0x04,
        Gravity = 0x06,
        /// raw angular velocity without preprocessing
        GyroscopeRaw = 0x15,
        /// angular velocity
        GyroscopeCalibrated = 0x02,
        /// angular velocity without bias compensation
        GyroscopeUncalibrated = 0x07,
        MagnetometerRaw = 0x16,
        MagnetometerCalibrated = 0x03,
        MagnetometerUncalibrated = 0x0f,
        RotationVector = 0x05,
        GameRotationVector = 0x08,
        GeomagneticRotationVector = 0x09,
        ArvrStabilizedRotationVector = 0x28,
        ArvrStabilizedGameRotationVector = 0x29,
    }

    pub struct Imu;

    #[derive(serde::Serialize, Debug)]
    #[repr(transparent)]
    pub struct In<T>(T);

    impl <S: Serializer, T: IoSerializeable<S>> IoSerializeable<S> for In<T> {
        type Metadata = T::Metadata;
        type Output = T::Output;
    }

    impl <T: Inputs> Inputs for In<T> {
        type Inputs<'a, N> = T::Inputs<'a, N> where Self: 'a, N: Node + 'a;
        fn inputs<'a, N: Node>(&'a self, node: &'a NodeT<N>) -> Self::Inputs<'a, N> {
            self.0.inputs(node)
        }
    }

    impl <T: Outputs> Outputs for Out<T> {
        type Outputs<'a, N> = T::Outputs<'a, N> where Self: 'a, N: Node + 'a;
        fn outputs<'a, N: Node>(&'a self, node: &'a NodeT<N>) -> Self::Outputs<'a, N> {
            self.0.outputs(node)
        }
    }

    impl <D: Deserializer, T: IoDeserializeable<D>> IoDeserializeable<D> for Out<T> {
        type Metadata = T::Metadata;
        type Output = T::Output;
    }


    #[derive(serde::Deserialize, Debug)]
    #[repr(transparent)]
    pub struct Out<T>(T);

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    pub struct ImuData {
        timestamp: Timestamp,
        device_timestamp: Timestamp,
        sequence: i32,
        packets: Vec<ImuPacket>,
    }

    impl NotAny for ImuData {}

    impl MetadataOnly for ImuData {}

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    struct ImuPacket {
        accelerometer: ImuAccelerometer,
        gyroscope: ImuGyroscope,
        magnetic_field: ImuMagneticField,
        rotation_vector: ImuRotationVector,
    }

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    struct ImuAccelerometer {
        x: f32,
        y: f32,
        z: f32,
        sequence: i32,
        accuracy: ImuAccuracy,
        timestamp: Timestamp,
        device_timestamp: Timestamp,
    }

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    struct ImuGyroscope {
        x: f32,
        y: f32,
        z: f32,
        sequence: i32,
        accuracy: ImuAccuracy,
        timestamp: Timestamp,
        device_timestamp: Timestamp,
    }

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    struct ImuMagneticField {
        x: f32,
        y: f32,
        z: f32,
        sequence: i32,
        accuracy: ImuAccuracy,
        timestamp: Timestamp,
        device_timestamp: Timestamp,
    }

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    struct ImuRotationVector {
        i: f32,
        j: f32,
        k: f32,
        real: f32,
        rvec_accuracy: f32,
        sequence: i32,
        accuracy: ImuAccuracy,
        timestamp: Timestamp,
        device_timestamp: Timestamp,
    }

    #[derive(serde_repr::Serialize_repr, serde_repr::Deserialize_repr, Debug)]
    #[repr(u8)]
    enum ImuAccuracy {
        Unreliable = 0,
        Low = 1,
        Medium = 2,
        High = 3,
    }

    #[derive(serde::Deserialize, serde::Serialize, Debug, PartialEq)]
    struct Timestamp {
        sec: i64,
        nsec: i64,
    }

    impl StaticIoDesc for In<ImuData> {
        const NAME: &str = "mockIn";
        const NODE_TYPE: NodeType = NodeType::SReceiver;
    }

    impl StaticIoDesc for Out<ImuData> {
        const NAME: &str = "out";
        const NODE_TYPE: NodeType = NodeType::MSender;
    }

    impl Node for Imu {
        type Input = Input<In<ImuData>, RnopSerializer>;
        type Output = Output<Out<ImuData>, RnopDeserializer>;
        type Properties = ImuProperties;
        const NAME: &str = "IMU";
    }

    pub struct Camera;

    // TODO: this also has support for dynamic outputs,
    // - create hashmap impl for (String, DynamicOutput/DynamicInput)
    impl Node for Camera {
        type Input = (Input<CameraInputControl, RnopSerializer>, Input<In<CameraFrame>, RnopSerializer>);
        type Output = (Output<Out<CameraFrame>, RnopDeserializer>, Vec<Output<Dynamic<CameraFrame>, RnopDeserializer>>);
        type Properties = CameraProperties;
        const NAME: &str = "Camera";
    }

    impl NodeT<Camera> {
        pub fn raw_camera_output(&self) -> OutputRef<'_, Camera, Out<CameraFrame>, RnopDeserializer> {
            self.output().0
        }

        pub fn requested_camera_outputs(&self) -> impl Iterator<Item = OutputRef<'_, Camera, Dynamic<CameraFrame>, RnopDeserializer>> {
            self.output().1
        }

        pub fn request_output(&mut self, capability: CameraCapability) -> usize {
            let len = self.output.1.len();
            let mut new_output = Output::<Dynamic<_>, _>::default();
            new_output.output.name = format!("{len}");
            self.output.1.push(new_output);
            self.properties.output_requests.push(capability);
            len
        }
    }

    impl DynamicGroup for CameraFrame {
        const GROUP: Option<&str> = Some("dynamicOutputs");
    }

    impl <T: IoDesc + IoDeserializeable<D>, D: Deserializer> Outputs for Vec<Output<T, D>> {
        type Outputs<'a, N: Node + 'a> = OutputRefIter<'a, T, D, N> where Self: 'a;
        fn outputs<'a, N: Node>(&'a self, node: &'a NodeT<N>) -> Self::Outputs<'a, N> {
            OutputRefIter {
                iter: self.as_slice().iter(),
                node,
            }
        }
    }

    impl <T: IoRegister> IoRegister for Vec<T> {
        fn register<'a, 'b>(&'a self, info: &mut IoInfo<'a, 'b>) {
            for it in self.iter() {
                it.register(info)
            }
        }
    }

    pub struct OutputRefIter<'a, T: IoDeserializeable<D> + IoDesc, D: Deserializer, N: Node> {
        iter: std::slice::Iter<'a, Output<T, D>>,
        node: &'a NodeT<N>,
    }

    impl <'a, T: IoDeserializeable<D> + IoDesc, D: Deserializer, N: Node> Iterator for OutputRefIter<'a, T, D, N> {
        type Item = OutputRef<'a, N, T, D>;

        fn next(&mut self) -> Option<Self::Item> {
            let next = self.iter.next()?;
            Some(next.outputs(self.node))
        }
    }

    impl <D: Deserializer> IoDeserializeable<D> for CameraFrame {
        type Metadata = Self;
        type Output = Vec<u8>;
    }

    impl <S: Serializer> IoSerializeable<S> for CameraFrame {
        type Metadata = Self;
        type Output = Vec<u8>;
    }

    impl StaticIoDesc for In<CameraFrame> {
        const NAME: &str = "mockIsp";
        const NODE_TYPE: NodeType = NodeType::SReceiver;
    }

    impl StaticIoDesc for Out<CameraFrame> {
        const NAME: &str = "raw";
        const NODE_TYPE: NodeType = NodeType::MSender;
    }

    impl StaticIoDesc for CameraFrame {
        // unimportant
        const NAME: &str = "";
        const NODE_TYPE: NodeType = NodeType::MSender;
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, Default, PartialEq)]
    pub struct CameraInputControl {
        cmd_mask: u64,
        pub auto_focus_mode: AutoFocusMode,
        lens_position: u8,
        lens_position_raw: f32,
        lens_pos_auto_infinity: u8,
        lens_pos_auto_macro: u8,
        pub exp_manual: ManualExposureParams,
        pub ae_region: RegionParams,
        pub af_region: RegionParams,
        pub awb_mode: AutoWhiteBalanceMode,
        pub scene_mode: SceneMode,
        pub anti_banding_mode: AntiBandingMode,
        pub ae_lock_mode: bool,
        pub awb_lock_mode: bool,
        pub capture_intent: CaptureIntent,
        pub control_mode: ControlMode,
        pub effect_mode: EffectMode,
        pub frame_sync_mode: FrameSyncMode,
        pub strobe_config: StrobeConfig,
        pub strobe_timings: StrobeTimings,
        pub ae_max_exposure_time_us: u32,
        pub exp_compensation: i8,
        pub brightness: i8,
        pub contrast: i8,
        pub saturation: i8,
        pub sharpness: u8,
        pub luma_denoise: u8,
        pub chroma_denoise: u8,
        pub wb_color_temp: u16,
        pub low_power_frame_burst: u8,
        pub low_power_frame_discard: u8,
        pub enable_hdr: bool,
        misc_controls: Vec<(String, String)>,
    }

    impl NotAny for CameraInputControl {}

    #[derive(serde_repr::Serialize_repr, serde_repr::Deserialize_repr, Debug, Default, PartialEq)]
    #[repr(u8)]
    enum AutoFocusMode {
        Off = 0,
        Auto = 1,
        Macro = 2,
        #[default]
        ContinuousVideo = 3,
        ContinuousPicture = 4,
        Edof = 5,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, Default, PartialEq)]
    pub struct ManualExposureParams {
        exposure_time_us: u32,
        sensitivity_iso: u32,
        frame_duration_us: u32,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, Default, PartialEq)]
    pub struct RegionParams {
        pub x: u16,
        pub y: u16,
        pub width: u16,
        pub height: u16,
        pub priority: u16,
    }

    #[derive(serde_repr::Serialize_repr, serde_repr::Deserialize_repr, Debug, Default, PartialEq)]
    #[repr(u8)]
    pub enum AutoWhiteBalanceMode {
        #[default]
        Off = 0,
        Auto = 1,
        Incandescent = 2,
        Flourescent = 3,
        WarmFlourescent = 4,
        Daylight = 5,
        CloudyDaylight = 6,
        Twilight = 7,
        Shade = 8,
    }

    #[derive(serde_repr::Serialize_repr, serde_repr::Deserialize_repr, Debug, Default, PartialEq)]
    #[repr(u8)]
    enum SceneMode {
        #[default]
        Unsupported = 0,
        FacePriority = 1,
        Action = 2,
        Portrait = 3,
        Landscape = 4,
        Night = 5,
        NightPortrait = 6,
        Theatre = 7,
        Beach = 8,
        Snow = 9,
        Sunset = 10,
        SteadyPhoto = 11,
        Fireworks = 12,
        Sports = 13,
        Party = 14,
        Candlelight = 15,
        Barcode = 16,
    }

    #[derive(serde_repr::Serialize_repr, serde_repr::Deserialize_repr, Debug, Default, PartialEq)]
    #[repr(u8)]
    enum AntiBandingMode {
        #[default]
        Off = 0,
        Mains50Hz = 1,
        Mains60Hz = 2,
        Auto = 3,
    }

    #[derive(serde_repr::Serialize_repr, serde_repr::Deserialize_repr, Debug, Default, PartialEq)]
    #[repr(u8)]
    enum CaptureIntent {
        #[default]
        Custom = 0,
        Preview = 1,
        StillCapture = 2,
        VideoRecord = 3,
        VideoSnapshot = 4,
        ZeroShutterLag = 5,
    }

    #[derive(serde_repr::Serialize_repr, serde_repr::Deserialize_repr, Debug, Default, PartialEq)]
    #[repr(u8)]
    enum ControlMode {
        #[default]
        Off = 0,
        Auto = 1,
        UseSceneMode = 2,
    }

    #[derive(serde_repr::Serialize_repr, serde_repr::Deserialize_repr, Debug, Default, PartialEq)]
    #[repr(u8)]
    enum EffectMode {
        #[default]
        Off = 0,
        Mono = 1,
        Negative = 2,
        Solarize = 3,
        Sepia = 4,
        Posterize = 5,
        Whiteboard = 6,
        Blackboard = 7,
        Aqua = 8,
    }

    #[derive(serde_repr::Serialize_repr, serde_repr::Deserialize_repr, Debug, Default, PartialEq)]
    #[repr(u8)]
    enum FrameSyncMode {
        #[default]
        Off = 0,
        Output = 1,
        Input = 2,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, Default, PartialEq)]
    pub struct StrobeConfig {
        pub(crate) enable: bool,
        active_level: u8,
        gpio_number: i8
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, Default, PartialEq)]
    pub struct StrobeTimings {
        exposure_begin_offset_us: i32,
        exposure_end_offset_us: i32,
        duration_us: u32,
    }

    impl StaticIoDesc for CameraInputControl {
        const NAME: &str = "inputControl";
        const NODE_TYPE: NodeType = NodeType::SReceiver;
    }

    trait MetadataOnly { }

    impl <S: Serializer, T: MetadataOnly + NotAny + Serialize<S>> IoSerializeable<S> for T {
        type Metadata = Self;
        type Output = ();
    }

    impl <D: Deserializer, T: MetadataOnly + NotAny + Deserialize<D>> IoDeserializeable<D> for T {
        type Metadata = Self;
        type Output = ();
    }

    impl MetadataOnly for CameraInputControl {}

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct CameraFrame {
        timestamp: Timestamp,
        device_timestamp: Timestamp,
        sequence: i32,
        fb: CameraSpecs,
        source_fb: CameraSpecs,
        settings: CameraSettings,
        category: u32,
        instance: u32,
        transformation: ImageTransformation,
    }

    impl NotAny for CameraFrame {}

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct ImageTransformation {
        transformation_mtx: [[f32; 3]; 3],
        transformation_mtx_inv: [[f32; 3]; 3],
        source_intrinsic_mtx: [[f32; 3]; 3],
        source_intrinsic_mtx_inv: [[f32; 3]; 3],
        distortion_model: crate::rpc::CameraModel,
        distortion_coefficients: Vec<f32>,
        src_width: u32,
        src_height: u32,
        width: u32,
        height: u32,
        src_crops: Vec<RotatedRect>,
        /*
        src_crop: RotatedRect,
        dst_crop: RotatedRect,
        crops_valid: bool,
        */
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct Vec2 {
        x: f32,
        y: f32,
        normalized: bool,
        has_normalized: bool,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct RotatedRect {
        center: Vec2,
        size: Vec2,
        angle: f32,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct CameraSettings {
        exposure_time_us: i32,
        sensitivity_iso: i32,
        lens_position: i32,
        wb_color_temp: i32,
        lens_position_raw: f32,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct CameraSpecs {
        ty: FrameType,
        width: u32,
        height: u32,
        stride: u32,
        bytes_pp: u32,
        // offsets into planes
        p1_offset: u32,
        p2_offset: u32,
        p3_offset: u32,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct CameraProperties {
        pub(crate) initial_control: CameraInputControl,
        pub(crate) board_socket: crate::rpc::CameraBoardSocket,
        pub(crate) sensor_type: crate::rpc::CameraSensorType,
        camera_name: String,
        image_orientation: crate::rpc::CameraImageOrientation,
        resolution_width: i32,
        resolution_height: i32,
        mock_isp_width: i32,
        mock_isp_height: i32,
        mock_isp_fps: f32,
        fps: f32,
        isp_3a_fps: i32,
        frames_pool_raw: i32,
        pool_raw_max_size: i32,
        frames_pool_isp: i32,
        pool_isp_max_size: i32,
        pool_video_frames: i32,
        pool_preview_frames: i32,
        pool_still_frames: i32,
        pool_outputs_frames: Option<i32>,
        pool_outputs_max_size: Option<i32>,
        pub(crate) output_requests: Vec<CameraCapability>
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct Capability<T> {
        pub range: Option<CapabilityRange<T>>,
    }

    impl <T> Capability<T> {
        pub fn new_none() -> Self {
            Self {
                range: None,
            }
        }

        pub fn new_single(t: T) -> Self {
            Self {
                range: Some(CapabilityRange::Single(t))
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub enum CapabilityRange<T> {
        Single(T),
        Pair(T, T),
        Collection(Vec<T>),
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct CameraCapability {
        pub(crate) size: Capability<(u32, u32)>,
        pub(crate) fps: Capability<f32>,
        pub(crate) ty: Option<FrameType>,
        pub(crate) resize_mode: FrameResize,
        pub(crate) enable_undistortion: Option<bool>,
        pub(crate) isp_output: bool,
    }

    #[derive(serde_repr::Serialize_repr, serde_repr::Deserialize_repr, Debug, PartialEq)]
    #[repr(u8)]
    pub enum FrameType {
        Yuv422i = 0,
        Yuv444p = 1,
        Yuv420p = 2,
        Yuv422p = 3,
        Yuv400p = 4,
        Rgba4444 = 5,
        Rgb161616 = 6,
        Rgb888p = 7,
        Bgr888p = 8,
        Rgb888i = 9,
        Bgr888i = 10,
        Lut2 = 11,
        Lut4 = 12,
        Lut16 = 13,
        Raw16 = 14,
        Raw14 = 15,
        Raw12 = 16,
        Raw10 = 17,
        Raw8 = 18,
        Pack10 = 19,
        Pack12 = 20,
        Yuv444i = 21,
        Nv12 = 22,
        Nv21 = 23,
        Bitstream = 24,
        Hdr = 25,
        RgbF16F16F16p = 26,
        BgrF16F16F16p = 27,
        RgbF16F16F16i = 28,
        BgrF16F16F16i = 29,
        Gray8 = 30,
        GrayF16 = 31,
        Raw32 = 32,
        None = 33,
    }

    #[derive(serde_repr::Serialize_repr, serde_repr::Deserialize_repr, Debug, PartialEq)]
    #[repr(u8)]
    pub enum FrameResize {
        Crop = 0,
        Stretch = 1,
        Letterbox = 2,
    }

    impl core::default::Default for CameraProperties {
        fn default() -> Self {
            Self {
                output_requests: vec![],
                initial_control: Default::default(),
                board_socket: Default::default(),
                sensor_type: Default::default(),
                camera_name: Default::default(),
                image_orientation: Default::default(),

                resolution_width: -1,
                resolution_height: -1,
                mock_isp_width: -1,
                mock_isp_height: -1,
                mock_isp_fps: -1.,
                fps: -1.,
                isp_3a_fps: 0,
                frames_pool_raw: 3,
                pool_raw_max_size: 1024 * 1024 * 10,
                frames_pool_isp: 3,
                pool_isp_max_size: 1024 * 1024 * 10,
                pool_video_frames: 4,
                pool_preview_frames: 4,
                pool_still_frames: 4,
                pool_outputs_frames: None,
                pool_outputs_max_size: None,
            }
        }
    }

    impl <T: IoDesc + IoDeserializeable<D>, D: Deserializer> Outputs for Output<T, D> {
        type Outputs<'a, N: Node + 'a> = OutputRef<'a, N, T, D> where Self: 'a;
        fn outputs<'a, N: Node>(&'a self, node: &'a NodeT<N>) -> Self::Outputs<'a, N> {
            OutputRef {
                node,
                output: self,
            }
        }
    }

    use std::collections::HashSet;

    #[derive(Debug)]
    pub struct Pipeline<'a> {
        current_node_id: u32,
        connections: HashSet<NodeConnection<'a>>,
        nodes: HashMap<u32, InternalNodeInfo<'a>>,
        current_io_id: u32,
        current_xlink_out_id: u32,
        properties: crate::rpc::GlobalProperties,
    }

    #[derive(PartialEq, Eq, Hash, Debug)]
    struct NodeConnection<'a> {
        input_id: u32,
        input_name: &'a str,
        input_group: Option<&'a str>,
        output_id: u32,
        output_name: &'a str,
        output_group: Option<&'a str>,
    }

    impl <'a> Pipeline<'a> {
        pub fn new() -> Self {
            Pipeline {
                current_node_id: 0,
                connections: HashSet::new(),
                nodes: HashMap::new(),
                current_io_id: 0,
                current_xlink_out_id: 0,
                properties: Default::default(),
            }
        }

        pub fn create_node<T: Node>(&mut self) -> NodeT<T> {
            self.create_node_with_properties(Default::default())
        }

        pub fn create_node_with_properties<T: Node>(&mut self, properties: T::Properties) -> NodeT<T> {
            let id = self.current_node_id;
            self.current_node_id += 1;
            NodeT {
                log_level: LogLevel::Off,
                //parent_id: 1,
                properties,
                _m: core::marker::PhantomData,
                input: Default::default(),
                output: Default::default(),
                id,
            }
        }

        pub fn link<I: IoSerializeable<S> + IoDesc, S: Serializer, O: IoDeserializeable<D> + IoDesc, D: Deserializer, N1: Node, N2: Node>(&mut self, output: OutputRef<'a, N1, O, D>, input: InputRef<'a, N2, I, S>) where (I::Metadata, O::Metadata): CompatibleLink, (S, D): CompatibleSerialization {
            let connection = NodeConnection {
                input_id: input.node.id,
                input_name: input.input.name(),
                input_group: input.input.group(),
                output_id: output.node.id,
                output_name: output.output.name(),
                output_group: output.output.group(),
            };

            self.connections.insert(connection);
            self.insert_node(&input.node);
            self.insert_node(&output.node);
        }

        fn insert_node<N: Node>(&mut self, node: &'a NodeT<N>) {
            register_node(node, &mut self.nodes, &mut self.current_io_id)
        }

        pub fn create_output_queue<O: IoDeserializeable<D> + IoDesc, D: Deserializer, N1: Node>(&mut self, output: OutputRef<'a, N1, O, D>, xlink: &'a mut NodeT<XLinkOut>)  -> OutputQueue<O::Metadata, D, queue_state::Pending> where ((), O::Metadata): CompatibleLink, (AnySerializer, D): CompatibleSerialization 
{
            let id = self.current_xlink_out_id; 
            self.current_xlink_out_id += 1;

            let name = format!("__x_{}_out", id);

            let stream_name = crate::StreamName::new(&name.as_bytes()).unwrap();

            xlink.properties.stream_name = name;

            self.link(output, xlink.input());

            OutputQueue {
                state: queue_state::Pending(stream_name),
                _pd: core::marker::PhantomData,
            }
        }

        pub fn build(self, device_id: &str) -> crate::rpc::PipelineSchema {
            let connections = self.connections.into_iter().map(|connection| {
                crate::rpc::NodeConnectionSchema {
                    output_id: connection.output_id as _,
                    output_group: connection.output_group.unwrap_or_default().to_string(),
                    output: connection.output_name.to_string(),

                    input_id: connection.input_id as _,
                    input_group: connection.input_group.unwrap_or_default().to_string(),
                    input: connection.input_name.to_string(),
                }
            }).collect::<Vec<_>>();

            let nodes = self.nodes.into_iter().map(|(id, node)| {

                let io_info = node.io_info.into_iter().map(|((group, name), io)| {
                    let group = group.unwrap_or_default();

                    ((group.to_string(), name.to_string()), crate::rpc::NodeIoInfo {
                        group: group.to_string(),
                        name: name.to_string(),
                        ty: io.ty,
                        blocking: io.conf.blocking,
                        queue_size: io.conf.queue_size.unwrap_or(8),
                        wait_for_message: io.conf.wait_for_message,
                        id: io.id,
                    })
                }).collect::<Vec<_>>();


                (id as _, crate::rpc::NodeObjInfo {
                    id: id as _,
                    parent_id: -1,
                    name: node.name.to_string(),
                    alias: node.alias.unwrap_or_default().to_string(),
                    device_id: device_id.to_string(),
                    properties: node.properties,
                    log_level: node.log_level,
                    io_info,
                    device_node: true,
                })
            }).collect::<Vec<_>>();

            crate::rpc::PipelineSchema {
                bridges: vec![],
                global_properties: self.properties,
                nodes,
                connections,
            }
        }
    }



    // need knowledge of all of these types of connections
    //
    // also later add a marker for real serializers/deserializers then make sure that those are
    // compatible as well as the types

    // when the pipe is created it should take a &mut to the device connection so that it can
    // create channels on the device once its created.

    /*
    pipe.link(logger.output(), out.input());
    // typed
    let queue1 = pipe.create_output_queue(logger.output(), out.input());
    // untyped
    let queue2 = out.create_output_queue();
    */



    //logger.output().link(out.input());

    impl <T: IoSerializeable<S>, S: Serializer> IoRegister for Input<T, S> where T: IoDesc {
        fn register<'a, 'b>(&'a self, info: &mut IoInfo<'a, 'b>) {
            info.push(T::GROUP, T::name(&self.input), T::node_type(&self.input), &self.conf)
        }
    }

    impl <T: IoDeserializeable<D>, D: Deserializer> IoRegister for Output<T, D> where T: IoDesc {
        fn register<'a, 'b>(&'a self, info: &mut IoInfo<'a, 'b>) {

            info.push(T::GROUP, T::name(&self.output), T::node_type(&self.output), &self.conf)
        }
    }

    impl <T: IoDeserializeable<D> + IoDesc, D: Deserializer> Output<T, D> {
        pub fn name(&self) -> &str {
            T::name(&self.output)
        }

        pub fn group(&self) -> Option<&str> {
            T::GROUP
        }

        pub fn node_type(&self) -> NodeType {
            T::node_type(&self.output)
        }
    }

    impl <T: IoSerializeable<S> + IoDesc, S: Serializer> Input<T, S> {
        pub fn name(&self) -> &str {
            T::name(&self.input)
        }

        pub fn group(&self) -> Option<&str> {
            T::GROUP
        }

        pub fn node_type(&self) -> NodeType {
            T::node_type(&self.input)
        }
    }

    #[derive(Debug)]
    struct IoDescConf {
        blocking: bool,
        queue_size: Option<i32>,
        wait_for_message: bool,
    }

    impl core::default::Default for IoDescConf {
        fn default() -> Self {
            Self {
                blocking: false,
                queue_size: Some(8),
                wait_for_message: false,
            }
        }
    }

    pub struct NodeT<P: Node> {
        log_level: LogLevel,
        // TODO: probably a better way to do parent id
        //parent_id: i64,
        properties: P::Properties,
        _m: core::marker::PhantomData<P>,
        input: P::Input,
        output: P::Output,
        id: u32,
    }

    impl <P: Node> NodeT<P> {
        pub fn input(&self) -> <P::Input as Inputs>::Inputs<'_, P> {
            self.input.inputs(self)
        }

        pub fn output(&self) -> <P::Output as Outputs>::Outputs<'_, P> {
            self.output.outputs(self)
        }

        pub fn properties(&self) -> &P::Properties {
            &self.properties
        }

        pub fn properties_mut(&mut self) -> &mut P::Properties {
            &mut self.properties
        }
    }

    // encoded structs

    use crate::rpc::{MemoryInfo, CpuUsage, ChipTemperature};

    #[derive(serde::Deserialize, Debug)]
    pub struct SystemInfo {
        ddr_memory_usage: MemoryInfo,
        cmx_memory_usage: MemoryInfo,
        leon_css_memory_usage: MemoryInfo,
        leon_mss_memory_usage: MemoryInfo,
        leon_css_cpu_usage: CpuUsage,
        leon_mss_cpu_usage: CpuUsage,
        chip_temperature: ChipTemperature,
    }

    impl NotAny for SystemInfo {}

    impl MetadataOnly for SystemInfo {}

    pub struct StereoDepth;

    impl Node for StereoDepth {
        type Input = StereoDepthInputs;
        type Output = StereoDepthOutputs;
        type Properties = StereoDepthProperties;
        const NAME: &str = "StereoDepth";
    }

    #[derive(serde::Serialize)]
    #[repr(transparent)]
    pub struct Numbered<T, const N: usize> {
        inner: T,
    }

    impl <T: Inputs, const C: usize> Inputs for Numbered<T, C> {
        type Inputs<'a, N> = T::Inputs<'a, N> where Self: 'a, N: Node + 'a;

        fn inputs<'a, N: Node>(&'a self, node: &'a NodeT<N>) -> Self::Inputs<'a, N> {
            self.inner.inputs(node)
        }
    }

    impl <D: Deserializer, T: IoDeserializeable<D>, const N: usize> IoDeserializeable<D> for Numbered<T, N> {
        type Metadata = T::Metadata;
        type Output = T::Output;
    }

    impl <S: Serializer, T: IoSerializeable<S>, const N: usize> IoSerializeable<S> for Numbered<T, N> {
        type Metadata = T::Metadata;
        type Output = T::Output;
    }


    impl <T: Outputs, const C: usize> Outputs for Numbered<T, C> {
        type Outputs<'a, N> = T::Outputs<'a, N> where Self: 'a, N: Node + 'a;

        fn outputs<'a, N: Node>(&'a self, node: &'a NodeT<N>) -> Self::Outputs<'a, N> {
            self.inner.outputs(node)
        }
    }

    #[derive(Default)]
    pub struct StereoDepthInputs {
        input_config: Input<In<StereoDepthConfig>, RnopSerializer>,
        input_align_to: Input<Numbered<CameraFrame, 0>, RnopSerializer>,
        left: Input<Numbered<CameraFrame, 1>, RnopSerializer>,
        right: Input<Numbered<CameraFrame, 2>, RnopSerializer>,
    }

    impl IoRegister for StereoDepthInputs {
        fn register<'a, 'b>(&'a self, info: &mut IoInfo<'a, 'b>) {
            let Self {
                input_config,
                input_align_to,
                left,
                right,
            } = self;

            input_config.register(info);
            input_align_to.register(info);
            left.register(info);
            right.register(info);
        }
    }

    pub struct StereoDepthInputRef<'a, N: Node> {
        pub input_config: InputRef<'a, N, In<StereoDepthConfig>, RnopSerializer>,
        pub input_align_to: InputRef<'a, N, Numbered<CameraFrame, 0>, RnopSerializer>,
        pub left: InputRef<'a, N, Numbered<CameraFrame, 1>, RnopSerializer>,
        pub right: InputRef<'a, N, Numbered<CameraFrame, 2>, RnopSerializer>,
    }

    impl Inputs for StereoDepthInputs {
        type Inputs<'a, N> = StereoDepthInputRef<'a, N> where Self: 'a, N: Node + 'a;

        fn inputs<'a, N: Node>(&'a self, node: &'a NodeT<N>) -> Self::Inputs<'a, N> {
            let Self {
                input_config,
                input_align_to,
                left,
                right,
            } = self;

            StereoDepthInputRef {
                input_config: input_config.inputs(node),
                input_align_to: input_align_to.inputs(node),
                left: left.inputs(node),
                right: right.inputs(node),
            }
        }
    }

    impl StaticIoDesc for Numbered<CameraFrame, 0> {
        const NAME: &str = "inputAlignTo";
        const NODE_TYPE: NodeType = NodeType::SReceiver;
    }

    impl StaticIoDesc for Numbered<CameraFrame, 1> {
        const NAME: &str = "left";
        const NODE_TYPE: NodeType = NodeType::SReceiver;
    }

    impl StaticIoDesc for Numbered<CameraFrame, 2> {
        const NAME: &str = "right";
        const NODE_TYPE: NodeType = NodeType::SReceiver;
    }

    #[derive(Default)]
    pub struct StereoDepthOutputs {
        depth: Output<Numbered<CameraFrame, 3>, RnopDeserializer>,
        disparity: Output<Numbered<CameraFrame, 4>, RnopDeserializer>,

        snyced_left: Output<Numbered<CameraFrame, 5>, RnopDeserializer>,
        synced_right: Output<Numbered<CameraFrame, 6>, RnopDeserializer>,

        rectified_left: Output<Numbered<CameraFrame, 7>, RnopDeserializer>,
        rectified_right: Output<Numbered<CameraFrame, 8>, RnopDeserializer>,

        config: Output<Out<StereoDepthConfig>, RnopDeserializer>,

        debug_lr_check_i1: Output<Numbered<CameraFrame, 9>, RnopDeserializer>,
        debug_lr_check_i2: Output<Numbered<CameraFrame, 10>, RnopDeserializer>,
        debug_ext_lr_check_i1: Output<Numbered<CameraFrame, 11>, RnopDeserializer>,
        debug_ext_lr_check_i2: Output<Numbered<CameraFrame, 12>, RnopDeserializer>,
        debug_cost_dump: Output<Numbered<CameraFrame, 13>, RnopDeserializer>,

        confidence_map: Output<Numbered<CameraFrame, 14>, RnopDeserializer>,
    }

    impl IoRegister for StereoDepthOutputs {
        fn register<'a, 'b>(&'a self, info: &mut IoInfo<'a, 'b>) {
            let Self {
                depth,
                disparity,
                snyced_left,
                synced_right,
                rectified_left,
                rectified_right,
                config,
                debug_lr_check_i1,
                debug_lr_check_i2,
                debug_ext_lr_check_i1,
                debug_ext_lr_check_i2,
                debug_cost_dump,
                confidence_map,
            } = self;

            depth.register(info);
            disparity.register(info);
            snyced_left.register(info);
            synced_right.register(info);
            rectified_left.register(info);
            rectified_right.register(info);
            config.register(info);
            debug_lr_check_i1.register(info);
            debug_lr_check_i2.register(info);
            debug_ext_lr_check_i1.register(info);
            debug_ext_lr_check_i2.register(info);
            debug_cost_dump.register(info);
            confidence_map.register(info);
        }
    }

    pub struct StereoDepthOutputRef<'a, N: Node> {
        pub depth: OutputRef<'a, N, Numbered<CameraFrame, 3>, RnopDeserializer>,
        pub disparity: OutputRef<'a, N, Numbered<CameraFrame, 4>, RnopDeserializer>,

        pub snyced_left: OutputRef<'a, N, Numbered<CameraFrame, 5>, RnopDeserializer>,
        pub synced_right: OutputRef<'a, N, Numbered<CameraFrame, 6>, RnopDeserializer>,

        pub rectified_left: OutputRef<'a, N, Numbered<CameraFrame, 7>, RnopDeserializer>,
        pub rectified_right: OutputRef<'a, N, Numbered<CameraFrame, 8>, RnopDeserializer>,

        pub config: OutputRef<'a, N, Out<StereoDepthConfig>, RnopDeserializer>,

        pub debug_lr_check_i1: OutputRef<'a, N, Numbered<CameraFrame, 9>, RnopDeserializer>,
        pub debug_lr_check_i2: OutputRef<'a, N, Numbered<CameraFrame, 10>, RnopDeserializer>,
        pub debug_ext_lr_check_i1: OutputRef<'a, N, Numbered<CameraFrame, 11>, RnopDeserializer>,
        pub debug_ext_lr_check_i2: OutputRef<'a, N, Numbered<CameraFrame, 12>, RnopDeserializer>,
        pub debug_cost_dump: OutputRef<'a, N, Numbered<CameraFrame, 13>, RnopDeserializer>,

        pub confidence_map: OutputRef<'a, N, Numbered<CameraFrame, 14>, RnopDeserializer>,
    }

    impl Outputs for StereoDepthOutputs {
        type Outputs<'a, N> = StereoDepthOutputRef<'a, N> where Self: 'a, N: Node + 'a;

        fn outputs<'a, N: Node>(&'a self, node: &'a NodeT<N>) -> Self::Outputs<'a, N> {
            let Self {
                depth,
                disparity,
                snyced_left,
                synced_right,
                rectified_left,
                rectified_right,
                config,
                debug_lr_check_i1,
                debug_lr_check_i2,
                debug_ext_lr_check_i1,
                debug_ext_lr_check_i2,
                debug_cost_dump,
                confidence_map,
            } = self;

            StereoDepthOutputRef {
                depth: depth.outputs(node),
                disparity: disparity.outputs(node),
                snyced_left: snyced_left.outputs(node),
                synced_right: synced_right.outputs(node),
                rectified_left: rectified_left.outputs(node),
                rectified_right: rectified_right.outputs(node),
                config: config.outputs(node),
                debug_lr_check_i1: debug_lr_check_i1.outputs(node),
                debug_lr_check_i2: debug_lr_check_i2.outputs(node),
                debug_ext_lr_check_i1: debug_ext_lr_check_i1.outputs(node),
                debug_ext_lr_check_i2: debug_ext_lr_check_i2.outputs(node),
                debug_cost_dump: debug_cost_dump.outputs(node),
                confidence_map: confidence_map.outputs(node),
            }
        }
    }

    impl StaticIoDesc for Numbered<CameraFrame, 3> {
        const NODE_TYPE: NodeType = NodeType::MSender;
        const NAME: &str = "depth";
    }

    impl StaticIoDesc for Numbered<CameraFrame, 4> {
        const NODE_TYPE: NodeType = NodeType::MSender;
        const NAME: &str = "disparity";
    }

    impl StaticIoDesc for Numbered<CameraFrame, 5> {
        const NODE_TYPE: NodeType = NodeType::MSender;
        const NAME: &str = "syncedLeft";
    }

    impl StaticIoDesc for Numbered<CameraFrame, 6> {
        const NODE_TYPE: NodeType = NodeType::MSender;
        const NAME: &str = "syncedRight";
    }

    impl StaticIoDesc for Numbered<CameraFrame, 7> {
        const NODE_TYPE: NodeType = NodeType::MSender;
        const NAME: &str = "rectifiedLeft";
    }

    impl StaticIoDesc for Numbered<CameraFrame, 8> {
        const NODE_TYPE: NodeType = NodeType::MSender;
        const NAME: &str = "rectifiedRight";
    }

    impl StaticIoDesc for Numbered<CameraFrame, 9> {
        const NODE_TYPE: NodeType = NodeType::MSender;
        const NAME: &str = "debugDispLrCheckIt1";
    }

    impl StaticIoDesc for Numbered<CameraFrame, 10> {
        const NODE_TYPE: NodeType = NodeType::MSender;
        const NAME: &str = "debugDispLrCheckIt2";
    }

    impl StaticIoDesc for Numbered<CameraFrame, 11> {
        const NODE_TYPE: NodeType = NodeType::MSender;
        const NAME: &str = "debugExtDispLrCheckIt1";
    }

    impl StaticIoDesc for Numbered<CameraFrame, 12> {
        const NODE_TYPE: NodeType = NodeType::MSender;
        const NAME: &str = "debugExtDispLrCheckIt2";
    }

    impl StaticIoDesc for Numbered<CameraFrame, 13> {
        const NODE_TYPE: NodeType = NodeType::MSender;
        const NAME: &str = "debugDispCostDump";
    }

    impl StaticIoDesc for Numbered<CameraFrame, 14> {
        const NODE_TYPE: NodeType = NodeType::MSender;
        const NAME: &str = "confidenceMap";
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct StereoDepthProperties {
        pub initial_config: StereoDepthConfig,
        pub depth_align_camera: crate::rpc::CameraBoardSocket,
        pub enable_rectification: bool,
        pub rectify_edge_fill_color: i32,
        pub width: Option<i32>,
        pub height: Option<i32>,
        pub out_width: Option<i32>,
        pub out_height: Option<i32>,
        pub keep_aspect_ratio: bool,
        pub mesh: RectificationMesh,
        pub enable_runtime_stereo_mode_switch: bool,
        pub frame_pool: i32,
        pub post_processing_shaves: i32,
        pub post_processing_memory_slices: i32,
        pub focal_length_from_calibration: bool,
        pub use_homography_rectification: Option<bool>,
        pub enable_frame_sync: bool,
        pub baseline: Option<f32>,
        pub focal_length: Option<f32>,
        pub disparity_to_depth_use_spec_translation: Option<bool>,
        pub rectification_use_spec_translation: Option<bool>,
        pub depth_alignment_use_spec_translation: Option<bool>,
        pub alpha_scaling: Option<f32>,
    }

    impl core::default::Default for StereoDepthProperties {
        fn default() -> Self {
            Self {
                initial_config: Default::default(),
                depth_align_camera: Default::default(),
                enable_rectification: true,
                rectify_edge_fill_color: 0,
                width: None,
                height: None,
                out_width: None,
                out_height: None,
                keep_aspect_ratio: true,
                mesh: Default::default(),
                enable_runtime_stereo_mode_switch: false,
                frame_pool: 3,
                post_processing_shaves: -1,
                post_processing_memory_slices: -1,
                focal_length_from_calibration: true,
                use_homography_rectification: None,
                enable_frame_sync: true,
                baseline: None,
                focal_length: None,
                disparity_to_depth_use_spec_translation: None,
                rectification_use_spec_translation: None,
                depth_alignment_use_spec_translation: None,
                alpha_scaling: None,
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct RectificationMesh {
        left_mesh_uri: String,
        right_mesh_uri: String,
        mesh_size: Option<u32>,
        step_width: u16,
        step_height: u16,
    }

    impl core::default::Default for RectificationMesh {
        fn default() -> Self {
            Self {
                left_mesh_uri: Default::default(),
                right_mesh_uri: Default::default(),
                mesh_size: None,
                step_width: 16,
                step_height: 16,
            }
        }
    }
    
    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct StereoDepthConfig {
        pub algorithm_control: AlgorithmControl,
        pub post_processing: PostProcessing,
        pub census_transorm: CensusTransform,
        pub cost_matching: CostMatching,
        pub cost_aggregation: CostAggregation,
        pub confidence_metrics: ConfidenceMetrics,
        pub filters_backend: crate::rpc::ProcessorType,
    }

    impl MetadataOnly for StereoDepthConfig {}

    impl StaticIoDesc for In<StereoDepthConfig> {
        const NAME: &str = "inputConfig";
        const NODE_TYPE: NodeType = NodeType::SReceiver;
    }

    impl StaticIoDesc for Out<StereoDepthConfig> {
        const NAME: &str = "outConfig";
        const NODE_TYPE: NodeType = NodeType::MSender;
    }

    impl NotAny for StereoDepthConfig {}

    impl core::default::Default for StereoDepthConfig {
        fn default() -> Self {
            Self {
                algorithm_control: Default::default(),
                post_processing: Default::default(),
                census_transorm: Default::default(),
                cost_matching: Default::default(),
                cost_aggregation: Default::default(),
                confidence_metrics: Default::default(),
                filters_backend: crate::rpc::ProcessorType::Cpu,
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct ConfidenceMetrics {
        occlusion_confidence_weight: u8,
        motion_vector_confidence_weight: u8,
        motion_vector_confidence_threshold: u8,
        flatness_confidence_weight: u8,
        flatness_confidence_threshold: u8,
        flatness_override: bool,
    }

    impl core::default::Default for ConfidenceMetrics {
        fn default() -> Self {
            Self {
                occlusion_confidence_weight: 20,
                motion_vector_confidence_weight: 4,
                motion_vector_confidence_threshold: 1,
                flatness_confidence_weight: 8,
                flatness_confidence_threshold: 2,
                flatness_override: false,
            }
        }
    }
    
    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct CostAggregation {
        division_factor: u8,
        horizontal_penalty_cost_p1: u16,
        horizontal_penalty_cost_p2: u16,
        vertical_penalty_cost_p1: u16,
        vertical_penalty_cost_p2: u16,
        p1_config: P1Config,
        p2_config: P2Config,
    }

    impl core::default::Default for CostAggregation {
        fn default() -> Self {
            Self {
                division_factor: 1,
                horizontal_penalty_cost_p1: 250,
                horizontal_penalty_cost_p2: 500,
                vertical_penalty_cost_p1: 250,
                vertical_penalty_cost_p2: 500,
                p1_config: Default::default(),
                p2_config: Default::default(),
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct P1Config {
        enable_adaptive: bool,
        default_value: u8,
        edge_value: u8,
        smooth_value: u8,
        edge_threshold: u8,
        smooth_threshold: u8,
    }

    impl core::default::Default for P1Config {
        fn default() -> Self {
            Self {
                enable_adaptive: true,
                default_value: 11,
                edge_value: 10,
                smooth_value: 22,
                edge_threshold: 15,
                smooth_threshold: 5,
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct P2Config {
        enable_adaptive: bool,
        default_value: u8,
        edge_value: u8,
        smooth_value: u8,
    }

    impl core::default::Default for P2Config {
        fn default() -> Self {
            Self {
                enable_adaptive: true,
                default_value: 33,
                edge_value: 22,
                smooth_value: 63,
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct CensusTransform {
        kernel_size: KernelSize,
        kernel_mask: u64,
        enable_mean_mode: bool,
        threshold: u32,
        noise_threshold_offset: i8,
        noise_threshold_scale: i8,
    }
    
    impl core::default::Default for CensusTransform {
        fn default() -> Self {
            Self {
                kernel_size: KernelSize::Auto,
                kernel_mask: 0,
                enable_mean_mode: true,
                threshold: 0,
                noise_threshold_offset: 1,
                noise_threshold_scale: 1,
            }
        }
    }

    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, PartialEq)]
    #[repr(i32)]
    pub enum KernelSize {
        Auto = -1,
        Kernel5x5 = 0,
        Kernel7x7 = 1,
        Kernel7x9 = 2,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct CostMatching {
        disparity_width: DisparityWidth,
        enable_companding: bool,
        invalid_disparity_value: u8,
        confidence_threshold: u8,
        enable_software_confidence_thresholding: bool,
        linear_equation_parameters: LinearEquationParameters,
    }
    impl core::default::Default for CostMatching {
        fn default() -> Self {
            Self {
                disparity_width: DisparityWidth::Disparity96,
                enable_companding: false,
                invalid_disparity_value: 0,
                confidence_threshold: 55,
                enable_software_confidence_thresholding: false,
                linear_equation_parameters: Default::default(),
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct LinearEquationParameters {
        alpha: u8,
        beta: u8,
        threshold: u8,
    }
    impl core::default::Default for LinearEquationParameters {
        fn default() -> Self {
            Self {
                alpha: 0,
                beta: 0,
                threshold: 127,
            }
        }
    }

    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, PartialEq)]
    #[repr(u32)]
    enum DisparityWidth {
        Disparity64 = 0,
        Disparity96 = 1,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct AlgorithmControl {
        pub depth_align: DepthAlign,
        pub depth_unit: DepthUnit,
        pub custom_depth_unit_multiplier: f32,
        pub enable_left_right_check: bool,
        pub enable_software_left_right_check: bool,
        pub enable_extended: bool,
        pub enable_subpixel: bool,
        pub left_right_check_threshold: i32,
        pub subpixel_fractional_bits: i32,
        pub disparity_shift: i32,
        pub center_alignment_shift_factor: Option<f32>,
        pub invalidate_edge_pixel_count: i32,
    }

    impl core::default::Default for AlgorithmControl {
        fn default() -> Self {
            Self {
                depth_align: DepthAlign::RectifiedLeft,
                depth_unit: DepthUnit::Millimeter,
                custom_depth_unit_multiplier: 1000.,
                enable_left_right_check: true,
                enable_software_left_right_check: false,
                enable_extended: false,
                enable_subpixel: true,
                left_right_check_threshold: 10,
                subpixel_fractional_bits: 5,
                disparity_shift: 0,
                center_alignment_shift_factor: None,
                invalidate_edge_pixel_count: 0,
            }
        }
    }
    
    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, PartialEq)]
    #[repr(i32)]
    pub enum DepthAlign {
        RectifiedRight = 0,
        RectifiedLeft = 1,
        Center = 2,
    }

    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, PartialEq)]
    #[repr(i32)]
    pub enum LengthUnit {
        Meter = 0,
        Centimeter = 1,
        Millimeter = 2,
        Inch = 3,
        Foot = 4,
        Custom = 5,
    }

    pub type DepthUnit = LengthUnit;

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    pub struct PostProcessing {
        filtering_order: [Filter; 5],
        median: MedianFilter,
        bilateral_sigma_value: i16,
        spacial_filter: SpatialFilter,
        temporal_filter: TemporalFilter,
        threshold_filter: ThresholdFilter,
        brightness_filter: BrightnessFilter,
        speckle_filter: SpeckleFilter,
        decimation_filter: DecimationFilter,
        hole_filling: HoleFilling,
        adaptive_median_filter: AdaptiveMedianFilter,
    }
    
    impl core::default::Default for PostProcessing {
        fn default() -> Self {
            Self {
                filtering_order: [Filter::Median, Filter::Decimation, Filter::Speckle, Filter::Spatial, Filter::Temporal],
                median: MedianFilter::Off,
                bilateral_sigma_value: 0,
                spacial_filter: Default::default(),
                temporal_filter: Default::default(),
                threshold_filter: Default::default(),
                brightness_filter: Default::default(),
                speckle_filter: Default::default(),
                decimation_filter: Default::default(),
                hole_filling: Default::default(),
                adaptive_median_filter: Default::default(),
            }
        }
    }

    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, PartialEq)]
    #[repr(i32)]
    pub enum Filter {
        None = 0,
        Decimation = 1,
        Speckle = 2,
        Median = 3,
        Spatial = 4,
        Temporal = 5,
    }

    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, PartialEq)]
    #[repr(i32)]
    pub enum MedianFilter {
        Off = 0,
        Kernel3x3 = 3,
        Kernel5x5 = 5,
        Kernel7x7 = 7,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct ThresholdFilter {
        min_range: i32,
        max_range: i32,
    }
    
    impl core::default::Default for ThresholdFilter {
        fn default() -> Self {
            Self {
                min_range: 0,
                max_range: u16::MAX as i32,
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct BrightnessFilter {
        min_brightness: i32,
        max_brightness: i32,
    }

    impl core::default::Default for BrightnessFilter {
        fn default() -> Self {
            Self {
                min_brightness: 0,
                max_brightness: (u8::MAX as i32) + 1,
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct SpeckleFilter {
        enable: bool,
        range: u32,
        difference_threshold: u32,
    }
    
    impl core::default::Default for SpeckleFilter {
        fn default() -> Self {
            Self {
                enable: false,
                range: 50,
                difference_threshold: 2,
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct DecimationFilter {
        decimation_factor: u32,
        decimation_mode: DecimationMode,
    }

    impl core::default::Default for DecimationFilter {
        fn default() -> Self {
            Self {
                decimation_factor: 1,
                decimation_mode: DecimationMode::PixelSkipping,
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct HoleFilling {
        enable: bool,
        high_confidence_threshold: u8,
        fill_confidence_threshold: u8,
        min_valid_disparity: u8,
        invalidate_disparities: bool,
    }

    impl core::default::Default for HoleFilling {
        fn default() -> Self {
            Self {
                enable: true,
                high_confidence_threshold: 210,
                fill_confidence_threshold: 200,
                min_valid_disparity: 1,
                invalidate_disparities: true,
            }
        }
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct AdaptiveMedianFilter {
        enable: bool,
        confidence_threshold: u8,
    }

    impl core::default::Default for AdaptiveMedianFilter {
        fn default() -> Self {
            Self {
                enable: true,
                confidence_threshold: 200,
            }
        }
    }

    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, PartialEq)]
    #[repr(i32)]
    enum DecimationMode {
        PixelSkipping = 0,
        NonZeroMedian = 1,
        NonZeroMean = 2,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct TemporalFilter {
        enable: bool,
        persistency_mode: PersistencyMode,
        alpha: f32,
        delta: i32,
    }

    impl core::default::Default for TemporalFilter {
        fn default() -> Self {
            Self {
                enable: false,
                persistency_mode: PersistencyMode::Valid2InLast4,
                alpha: 0.4,
                delta: 3,
            }
        }
    }

    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, PartialEq)]
    #[repr(i32)]
    enum PersistencyMode {
        Off = 0,
        Valid8OutOf8 = 1,
        Valid2InLast3 = 2,
        Valid2InLast4 = 3,
        Valid2OutOf8 = 4,
        Valid1InLast2 = 5,
        Valid1InLast5 = 6,
        Valid1InLast8 = 7,
        Indefinitely = 8,
    }

    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct SpatialFilter {
        enable: bool,
        hole_filling_radius: u8,
        alpha: f32,
        delta: i32,
        num_iterations: i32,
    }
    
    impl core::default::Default for SpatialFilter {
        fn default() -> Self {
            Self {
                enable: false,
                hole_filling_radius: 2,
                alpha: 0.5,
                delta: 3,
                num_iterations: 1,
            }
        }
    }

    #[test]
    fn pipeline_link() {
        let mut pipe = Pipeline::new();
        let logger = pipe.create_node::<SystemLogger>();
        let out = pipe.create_node::<XLinkOut>();

        pipe.link(logger.output(), out.input());

        assert_eq!(pipe.connections.len(), 1);
        assert_eq!(pipe.nodes.len(), 2);
    }

    #[test]
    fn pipeline_schema() {
        let mut pipe = Pipeline::new();
        let logger = pipe.create_node::<SystemLogger>();
        let mut out = pipe.create_node::<XLinkOut>();

        pipe.create_output_queue(logger.output(), &mut out);

        const DEVICE_ID: &str = "device_id";

        let schema = pipe.build(DEVICE_ID);
    }

    #[test]
    fn pipeline_schema_dynamic() {
        let mut pipe = Pipeline::new();
        let mut camera = pipe.create_node::<Camera>();
        let mut out = pipe.create_node::<XLinkOut>();

        let id = camera.request_output(CameraCapability {
            size: Capability::new_single((640, 400)),
            fps: Capability::new_none(),
            ty: None,
            enable_undistortion: None,
            isp_output: true,
            resize_mode: FrameResize::Crop,
        });

        let output = camera.requested_camera_outputs().next().unwrap();

        pipe.create_output_queue(output, &mut out);

        assert_eq!(pipe.connections.len(), 1);
        assert_eq!(pipe.nodes.len(), 2);
    }

    #[test]
    fn pipeline_schema_stereo() {
        let mut pipe = Pipeline::new();
        let mut camera_left = pipe.create_node::<Camera>();
        camera_left.properties_mut().board_socket = crate::rpc::CameraBoardSocket::B;

        camera_left.request_output(CameraCapability {
            size: Capability::new_single((1920, 1200)),
            fps: Capability::new_none(),
            ty: None,
            enable_undistortion: None,
            isp_output: true,
            resize_mode: FrameResize::Crop,
        });

        let cam_left = camera_left.requested_camera_outputs().next().unwrap();

        let mut camera_right = pipe.create_node::<Camera>();
        camera_right.properties_mut().board_socket = crate::rpc::CameraBoardSocket::C;

        camera_right.request_output(CameraCapability {
            size: Capability::new_single((1920, 1200)),
            fps: Capability::new_none(),
            ty: None,
            enable_undistortion: None,
            isp_output: true,
            resize_mode: FrameResize::Crop,
        });

        let cam_right = camera_right.requested_camera_outputs().next().unwrap();


        let mut stereo = pipe.create_node::<StereoDepth>();

        pipe.link(cam_left, stereo.input().left);
        pipe.link(cam_right, stereo.input().right);

        let mut out = pipe.create_node::<XLinkOut>();

        let xlink_out = pipe.create_output_queue(stereo.output().disparity, &mut out);

        assert_eq!(pipe.connections.len(), 3);
        assert_eq!(pipe.nodes.len(), 4);
        let schema = pipe.build("DEVICE");
    }

    #[test]
    fn pipeline_queue() {
        let mut pipe = Pipeline::new();
        let logger = pipe.create_node::<SystemLogger>();
        let mut out = pipe.create_node::<XLinkOut>();
        let out2 = pipe.create_node::<XLinkOut>();

        pipe.create_output_queue(logger.output(), &mut out);

        assert_eq!(pipe.connections.len(), 1);
        assert_eq!(pipe.nodes.len(), 2);
    }


    #[test]
    fn pipeline_properties() {
        // xlinkout
        {
            let mut w = vec![];
            let mut properties = XLinkOutProperties::default();

            properties.stream_name = String::from("__x_0_out");

            let original = vec![185, 5, 136, 0, 0, 128, 191, 189, 9, 95, 95, 120, 95, 48, 95, 111, 117, 116, 0, 255, 255];

            let old = RnopDeserializer::deserialize::<XLinkOutProperties>(&original).unwrap();

            RnopSerializer::serialize(&properties, &mut w).unwrap();

            assert_eq!(w, original);
        }
        // systemlogger
        {
            let mut w = vec![];
            let properties = SystemLoggerProperties::default();
            let original = vec![185, 1, 136, 0, 0, 128, 63];

            // FIXME: this will error (no idea why rn)
            //let old = RnopDeserializer::deserialize::<XLinkOutProperties>(&original).unwrap();
            RnopSerializer::serialize(&properties, &mut w).unwrap();
            assert_eq!(w, original);
        }
        // imu
        {
            let original = vec![185,4,186,2,185,5,0,0,0,129,224,1,20,185,5,0,0,0,129,144,1,21,1,10,0];
            let old = rnop::Value::parse(&original).unwrap();

            let mut props = crate::pipeline::ImuProperties::default();

            props.enable_sensor(crate::pipeline::ImuSensorKind::Accelerometer, 480);
            props.enable_sensor(crate::pipeline::ImuSensorKind::GyroscopeCalibrated, 400);

            let v = rnop::to_value(&props).unwrap();

            //panic!("{old:?}\n{v:?}");

            //let old = rnop::to_value(&original);
            //panic!("{old:?}");
            //let old = RnopDeserializer::deserialize::<ImuProperties>(&original).unwrap();
            //panic!("{old:?}");
        }
        // camera
        {
            let original = vec![185,22,185,33,0,3,0,136,0,0,0,0,0,0,185,3,0,0,0,185,5,0,0,0,0,0,185,5,0,0,0,0,0,0,0,0,0,0,0,0,0,0,185,3,0,0,0,185,3,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,186,0,0,255,189,0,255,255,255,255,255,136,0,0,128,191,136,0,0,128,191,0,3,134,0,0,160,0,3,134,0,0,160,0,4,4,4,190,190,186,1,185,6,185,1,184,0,186,2,129,128,7,129,176,4,185,1,190,190,0,190,0];


            let mut def = CameraProperties::default();

            def.initial_control.ae_lock_mode = true;
            def.initial_control.awb_lock_mode = true;
            def.initial_control.strobe_config.enable = true;
            def.initial_control.enable_hdr = true;
            def.board_socket = crate::rpc::CameraBoardSocket::A;


            def.output_requests.push(CameraCapability {
                size: Capability::new_single((1920, 1200)),
                fps: Capability::new_none(),
                ty: None,
                enable_undistortion: None,
                isp_output: true,
                resize_mode: FrameResize::Crop,
            });
            /*
            let mut w = vec![];
            RnopSerializer::serialize(&def, &mut w).unwrap();
            */

            //assert_eq!(original, w);

            // TODO:: compare the ser from the default properties to what was given
            // - might be an issue with not providing an ouptut_request


            let o1 = vec![185,6,185,1,184,0,186,2,129,128,7,129,176,4,185,1,190,190,0,190,0];
            let o2 = RnopDeserializer::deserialize::<CameraCapability>(&o1).unwrap();

            let old = rnop::Value::parse(&original).unwrap();
            //panic!("{old:?}");
            
            //let props = CameraProperties
            let old = RnopDeserializer::deserialize::<CameraProperties>(&original).unwrap();

            assert_eq!(def, old);

            //panic!("camera props: {old:?}");
        }

        {
            /*
            let ours = vec![185, 23, 185, 7, 185, 12, 1, 2, 136, 0, 0, 122, 68, 1, 0, 0, 1, 10, 5, 0, 190, 0, 185, 11, 186, 5, 3, 1, 2, 4, 5, 0, 0, 185, 5, 0, 2, 136, 0, 0, 0, 63, 3, 1, 185, 4, 0, 3, 136, 205, 204, 204, 62, 3, 185, 2, 0, 134, 255, 255, 0, 0, 185, 2, 0, 133, 255, 0, 185, 3, 0, 50, 2, 185, 2, 1, 0, 185, 5, 1, 128, 210, 128, 200, 1, 1, 185, 2, 1, 128, 200, 185, 6, 255, 0, 1, 0, 1, 1, 185, 6, 1, 0, 0, 55, 0, 185, 3, 0, 0, 127, 185, 7, 1, 128, 250, 129, 244, 1, 128, 250, 129, 244, 1, 185, 6, 1, 11, 10, 22, 15, 5, 185, 4, 1, 33, 22, 63, 185, 6, 20, 4, 1, 8, 2, 0, 2, 255, 1, 0, 190, 190, 190, 190, 1, 185, 5, 189, 0, 189, 0, 190, 16, 16, 0, 3, 255, 255, 1, 190, 1, 190, 190, 190, 190, 190, 190];
            */

            let theirs = vec![185,23,185,7,185,12,1,2,136,0,0,122,68,1,0,1,1,10,5,0,190,0,185,11,186,5,3,1,2,4,5,0,0,185,5,0,2,136,0,0,0,63,3,1,185,4,0,3,136,205,204,204,62,3,185,2,0,134,255,255,0,0,185,2,0,133,0,1,185,3,0,50,2,185,2,1,0,185,5,1,128,210,128,200,1,1,185,2,1,128,200,185,6,255,0,1,0,1,1,185,6,1,0,0,55,0,185,3,0,2,127,185,7,1,128,250,129,244,1,128,250,129,244,1,185,6,1,11,10,22,15,5,185,4,1,33,22,63,185,6,20,4,1,8,2,0,2,255,1,0,190,190,190,190,1,185,5,189,0,189,0,190,16,16,0,3,255,255,1,190,1,190,190,190,190,190,190];

            let old = RnopDeserializer::deserialize::<StereoDepthProperties>(&theirs).unwrap();

            let mut props = StereoDepthProperties::default();
            //props.initial_config.algorithm_control.enable_extended = false;
            props.initial_config.algorithm_control.enable_left_right_check = false;
            props.initial_config.algorithm_control.enable_software_left_right_check = true;
            props.initial_config.algorithm_control.enable_subpixel = false;
            props.initial_config.post_processing.spacial_filter.enable = true;
            props.initial_config.post_processing.temporal_filter.enable = true;
            props.initial_config.post_processing.speckle_filter.enable = true;
            props.initial_config.post_processing.hole_filling.enable = false;
            props.initial_config.post_processing.hole_filling.invalidate_disparities = false;
            props.initial_config.post_processing.adaptive_median_filter.enable = false;

            assert_eq!(props.initial_config.post_processing, old.initial_config.post_processing);

            // right camera
            let cam_1 = vec![185,22,185,33,0,3,0,136,0,0,0,0,0,0,185,3,0,0,0,185,5,129,16,15,129,228,125,129,86,91,0,3,185,5,0,0,129,102,111,118,0,0,0,0,0,0,0,0,0,0,185,3,0,0,0,185,3,0,0,0,0,0,0,132,129,2,0,0,0,0,80,84,0,186,0,2,255,189,0,255,255,255,255,255,136,0,0,128,191,136,0,0,128,191,0,3,134,0,0,160,0,3,134,0,0,160,0,4,4,4,190,190,186,1,185,6,185,1,184,0,186,2,129,128,7,129,176,4,185,1,190,190,0,190,0];

            let right = RnopDeserializer::deserialize::<CameraProperties>(&cam_1).unwrap();

            let mut cam_r = CameraProperties::default();
            cam_r.initial_control.ae_region.x = 3856;
            cam_r.initial_control.ae_region.y = 32228;
            cam_r.initial_control.ae_region.width = 23382;
            cam_r.initial_control.ae_region.height = 0;
            cam_r.initial_control.ae_region.priority = 3;

            cam_r.initial_control.af_region.width = 28518;
            cam_r.initial_control.af_region.height = 118;

            cam_r.initial_control.ae_lock_mode = true;
            cam_r.initial_control.awb_lock_mode = true;

            cam_r.initial_control.strobe_config.enable = true;
            cam_r.initial_control.contrast = -127;
            cam_r.initial_control.saturation = 2;
            cam_r.initial_control.low_power_frame_burst = 80;
            cam_r.initial_control.low_power_frame_discard = 84;
            cam_r.initial_control.enable_hdr = true;

            cam_r.board_socket = crate::rpc::CameraBoardSocket::C;

            cam_r.output_requests.push(CameraCapability {
                size: Capability::new_single((1920, 1200)),
                fps: Capability::new_none(),
                ty: None,
                enable_undistortion: None,
                isp_output: true,
                resize_mode: FrameResize::Crop,
            });

            assert_eq!(cam_r, right);

            // left cam


            let cam_2 = vec![185,22,185,33,0,3,0,136,0,0,0,0,0,0,185,3,0,0,0,185,5,0,0,0,0,0,185,5,129,45,19,0,0,0,129,45,19,0,0,0,0,0,0,128,250,128,165,9,185,3,0,0,0,185,3,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,186,0,1,255,189,0,255,255,255,255,255,136,0,0,128,191,136,0,0,128,191,0,3,134,0,0,160,0,3,134,0,0,160,0,4,4,4,190,190,186,1,185,6,185,1,184,0,186,2,129,128,7,129,176,4,185,1,190,190,0,190,0];
            let val = rnop::Value::parse(&cam_2);
            panic!("{val:?}");

            let left = RnopDeserializer::deserialize::<CameraProperties>(&cam_2).unwrap();

            let mut cam_l = CameraProperties::default();
            cam_l.initial_control.af_region.x = 4909;
            cam_l.initial_control.af_region.priority = 4909;

            cam_l.initial_control.ae_lock_mode = true;
            cam_l.initial_control.awb_lock_mode = true;

            cam_l.initial_control.strobe_config.enable = true;
            //cam_l.initial_control.contrast = -127;
            //cam_l.initial_control.saturation = 2;
            //cam_l.initial_control.low_power_frame_burst = 80;
            //cam_l.initial_control.low_power_frame_discard = 84;
            //cam_l.initial_control.enable_hdr = true;

            cam_l.board_socket = crate::rpc::CameraBoardSocket::B;

            cam_l.output_requests.push(CameraCapability {
                size: Capability::new_single((1920, 1200)),
                fps: Capability::new_none(),
                ty: None,
                enable_undistortion: None,
                isp_output: false,
                resize_mode: FrameResize::Crop,
            });

            //assert_eq!(cam_l, left);
        }
    }

    #[test]
    fn imu_data_decode() {
        let data = vec![185, 4, 185, 2, 10, 134, 88, 195, 149, 33, 185, 2, 10, 134, 88, 195, 149, 33, 0, 186, 1, 185, 4, 185, 7, 136, 0, 128, 139, 63, 136, 0, 48, 29, 65, 136, 0, 0, 64, 189, 133, 28, 5, 2, 185, 2, 10, 134, 88, 195, 149, 33, 185, 2, 10, 134, 88, 195, 149, 33, 185, 7, 136, 0, 0, 0, 0, 136, 0, 0, 0, 0, 136, 0, 0, 0, 0, 133, 181, 7, 3, 185, 2, 10, 134, 208, 254, 115, 33, 185, 2, 10, 134, 208, 254, 115, 33, 185, 7, 136, 0, 0, 0, 0, 136, 0, 0, 0, 0, 136, 0, 0, 0, 0, 0, 0, 185, 2, 0, 0, 185, 2, 0, 0, 185, 9, 136, 0, 0, 0, 0, 136, 0, 0, 0, 0, 136, 0, 0, 0, 0, 136, 0, 0, 0, 0, 136, 0, 0, 0, 0, 0, 0, 185, 2, 0, 0, 185, 2, 0, 0, 19, 0, 0, 0, 161, 0, 0, 0, 171, 205, 239, 1, 35, 69, 103, 137, 18, 52, 86, 120, 154, 188, 222, 240];


        let v = <ImuData as Deserialize<RnopDeserializer>>::deserialize(&data).unwrap();
        //panic!("{v:?}");
        //let v = rnop::Value::parse(&data).unwrap();
        //panic!("{v:?}");
    }
}

mod rpc {
    use std::collections::HashMap;
    use crate::ConnectionStream;

    pub struct Rpc<'a> {
        inner: &'a mut ConnectionStream,
    }

    
    impl <'a> Rpc<'a> {
        pub fn new(stream: &'a mut ConnectionStream) -> Self {
            Self {
                inner: stream,
            }
        }

        // what a silly hash function
        fn hash_method(method_name: &str) -> u64 {
            let mut h: u64 = 1125899906842597;
            for b in method_name.as_bytes() {
                h = h.wrapping_mul(31).wrapping_add(*b as u64);
            }
            h
        }

        const VERSION: u32 = 1;
        const REQUEST: u32 = 1;
        const RESPONSE: u32 = 2;

        async fn call_untyped(&mut self, method: &str, params: impl Iterator<Item = rmpv::Value>) -> Result<rmpv::Value, rmpv::Value> {
            let msg = rmpv::Value::Array([Self::VERSION.into(), Self::REQUEST.into(), Self::hash_method(method).into(), rmpv::Value::Array(params.collect())].into_iter().collect());

            let mut data = vec![];
            rmpv::encode::write_value(&mut data, &msg).unwrap();

            use crate::Event;
            let ev = Event::write(self.inner.write.id, b"", &data);

            self.inner.write(ev, data).await;
            let mut bytes = std::io::Cursor::new(self.inner.read().await);
            let res = rmpv::decode::read_value(&mut bytes).unwrap();

            let rmpv::Value::Array(mut items) = res else {
                panic!();
            };

            if items.len() == 3 {
                items.push(rmpv::Value::Nil);
            }

            let [version, msg_ty, status, res]: [rmpv::Value; 4] = items.try_into().unwrap();

            if version != Self::VERSION.into() {
                panic!()
            }

            if msg_ty != Self::RESPONSE.into() {
                panic!()
            }

            if status == 1.into() {
                Ok(res)
            } else if status == 0.into() {
                Err(res)
            } else {
                panic!()
            }
        }

        async fn call<T: serde::de::DeserializeOwned, E: serde::de::DeserializeOwned>(&mut self, method: &str, params: impl Iterator<Item = rmpv::Value>) -> Result<T, E> {
            self.call_untyped(method, params).await.map(|o| serde_rmpv::from_value(&o).unwrap()).map_err(|e| serde_rmpv::from_value(&e).unwrap())
        }

        pub async fn is_running(&mut self) -> bool {
            self.call::<_, String>("isRunning", [].into_iter()).await.unwrap()
        }

        pub async fn enable_crash_dump(&mut self, enable: bool) -> Result<(), String>{
            self.call_untyped("enableCrashDump", [enable.into()].into_iter()).await.map_err(|e| serde_rmpv::from_value::<String>(&e).unwrap())?;
            Ok(())
        }

        pub async fn mxid(&mut self) -> Result<String, String> {
            self.call("getMxId", [].into_iter()).await
        }

        pub async fn connected_cameras(&mut self) -> Result<Vec<CameraBoardSocket>, String> {
            self.call("getConnectedCameras", [].into_iter()).await
        }

        pub async fn connection_interfaces(&mut self) -> Result<Vec<ConnectionInterface>, String> {
            self.call("getConnectionInterfaces", [].into_iter()).await
        }

        pub async fn connected_camera_features(&mut self) -> Result<Vec<CameraFeatures>, String> {
            self.call("getConnectedCameraFeatures", [].into_iter()).await
        }
        /*
        async fn connected_camera_features(&mut self) -> Result<rmpv::Value, rmpv::Value> {
            self.call_untyped("getConnectedCameraFeatures", [].into_iter()).await
        }
        */

        pub async fn stereo_pairs(&mut self) -> Result<Vec<StereoPair>, String> {
            self.call("getStereoPairs", [].into_iter()).await
        }

        pub async fn camera_sensor_names(&mut self) -> Result<HashMap<CameraBoardSocket, String>, String> {
            let names = self.call_untyped("getCameraSensorNames", [].into_iter()).await.map_err(|e| serde_rmpv::from_value::<String>(&e).unwrap())?;

            let rmpv::Value::Array(names) = names else {
                panic!()
            };

            let mut map = HashMap::new();

            for pair in names {
                let rmpv::Value::Array(vals) = pair else {
                    panic!();
                };

                let [sock, name]: [rmpv::Value; 2] = vals.try_into().unwrap() else {
                    panic!();
                };
                let sock = serde_rmpv::from_value(&sock).unwrap();
                let name = serde_rmpv::from_value(&name).unwrap();
                map.insert(sock, name);
            }
            Ok(map)
        }
        /*
        async fn camera_sensor_names(&mut self) -> Result<HashMap<CameraBoardSocket, String>, String> {
            self.call("getCameraSensorNames", [].into_iter()).await
        }
        */

        pub async fn connected_imu(&mut self) -> Result<String, String> {
            self.call("getConnectedIMU", [].into_iter()).await
        }

        pub async fn crash_device(&mut self) -> Result<rmpv::Value, rmpv::Value> {
            self.call_untyped("crashDevice", [].into_iter()).await
        }

        pub async fn external_strobe_enable(&mut self, enable: bool) -> Result<rmpv::Value, rmpv::Value> {
            self.call_untyped("setExternalStrobeEnable", [enable.into()].into_iter()).await
        }

        pub async fn imu_firmware_version(&mut self) -> Result<String, String> {
            self.call("getIMUFirmwareVersion", [].into_iter()).await
        }

        pub async fn embedded_imu_firmware_version(&mut self) -> Result<String, String> {
            self.call("getEmbeddedIMUFirmwareVersion", [].into_iter()).await
        }

        pub async fn ddr_memory_usage(&mut self) -> Result<MemoryInfo, String> {
            self.call("getDdrUsage", [].into_iter()).await
        }
        
        pub async fn cmx_memory_usage(&mut self) -> Result<MemoryInfo, String> {
            self.call("getCmxUsage", [].into_iter()).await
        }

        pub async fn leon_css_heap_usage(&mut self) -> Result<MemoryInfo, String> {
            self.call("getLeonCssHeapUsage", [].into_iter()).await
        }

        pub async fn leon_mss_heap_usage(&mut self) -> Result<MemoryInfo, String> {
            self.call("getLeonMssHeapUsage", [].into_iter()).await
        }

        pub async fn chip_temperature(&mut self) -> Result<ChipTemperature, String> {
            self.call("getChipTemperature", [].into_iter()).await
        }

        pub async fn leon_css_cpu_usage(&mut self) -> Result<CpuUsage, String> {
            self.call("getLeonCssCpuUsage", [].into_iter()).await
        }

        pub async fn leon_mss_cpu_usage(&mut self) -> Result<CpuUsage, String> {
            self.call("getLeonMssCpuUsage", [].into_iter()).await
        }

        pub async fn process_memory_usage(&mut self) -> Result<i64, String> {
            self.call("getProcessMemoryUsage", [].into_iter()).await
        }

        pub async fn usb_speed(&mut self) -> Result<UsbSpeed, String> {
            self.call("getUsbSpeed", [].into_iter()).await
        }

        pub async fn is_neural_depth_supported(&mut self) -> Result<bool, String> {
            self.call("isNeuralDepthSupported", [].into_iter()).await
        }

        pub async fn is_pipeline_running(&mut self) -> Result<bool, String> {
            self.call("isPipelineRunning", [].into_iter()).await
        }

        pub async fn set_log_level(&mut self, log_level: LogLevel) -> Result<rmpv::Value, rmpv::Value> {
            self.call_untyped("setLogLevel", [(log_level as u8).into()].into_iter()).await
        }

        pub async fn set_node_log_level(&mut self, node_id: i64, log_level: LogLevel) -> Result<rmpv::Value, rmpv::Value> {
            self.call_untyped("setNodeLogLevel", [node_id.into(), (log_level as u8).into()].into_iter()).await
        }

        pub async fn log_level(&mut self) -> Result<u32, String> {
            self.call("getLogLevel", [].into_iter()).await
        }

        pub async fn node_log_level(&mut self, node_id: u64) -> Result<u32, String> {
            self.call("getNodeLogLevel", [node_id.into()].into_iter()).await
        }

        pub async fn xlink_chunk_size(&mut self) -> Result<i32, String> {
            self.call("getXLinkChunkSize", [].into_iter()).await
        }

        pub async fn set_ir_laser_dot_projector_intensity(&mut self, intensity: f32, mask: i32) -> Result<i32, String> {
            self.call("setIrLaserDotProjectorBrightness", [intensity.into(), mask.into(), true.into()].into_iter()).await
        }

        pub async fn set_ir_floodlight_intensity(&mut self, intensity: f32, mask: i32) -> Result<i32, String> {
            self.call("setIrFloodLightBrightness", [intensity.into(), mask.into(), true.into()].into_iter()).await
        }

        pub async fn ir_drivers(&mut self) -> Result<Option<(String, i32, i32)>, String> {
            let items = self.call_untyped("getIrDrivers", [].into_iter()).await.map_err(|e| serde_rmpv::from_value::<String>(&e).unwrap())?;
            let rmpv::Value::Array(items) = items else {
                panic!();
            };

            if items.is_empty() {
                return Ok(None)
            }

            let [name, v1, v2]: [rmpv::Value; 3] = items.try_into().unwrap();

            Ok(Some((serde_rmpv::from_value(&name).unwrap(), serde_rmpv::from_value(&v1).unwrap(), serde_rmpv::from_value(&v2).unwrap())))
        }

        /*
        async fn crash_dump(&mut self, clear: bool) -> Result<rmpv::Value, rmpv::Value> {
            self.call_untyped("getCrashDump", [clear.into()].into_iter()).await
        }
        */
        pub async fn crash_dump(&mut self, clear: bool) -> Result<CrashDump, String> {
            self.call("getCrashDump", [clear.into()].into_iter()).await
        }

        pub async fn has_crash_dump(&mut self) -> Result<bool, String> {
            self.call("hasCrashDump", [].into_iter()).await
        }

        const DEFAULT_TIMESYNC_PERIOD: std::time::Duration = std::time::Duration::from_millis(1000);
        const DEFAULT_TIMESYNC_SAMPLE_COUNT: u32 = 1000;
        const DEFAULT_TIMESYNC_RANDOM: bool = false;
        pub async fn set_timesync(&mut self, period: Option<std::time::Duration>, sample_count: Option<u32>, random: Option<bool>, enable: bool) -> Result<rmpv::Value, rmpv::Value> {
            let (period, sample_count, random) = if !enable {
                (std::time::Duration::from_millis(1000), 0, false)
            } else {
                (period.unwrap_or(Self::DEFAULT_TIMESYNC_PERIOD), sample_count.unwrap_or(Self::DEFAULT_TIMESYNC_SAMPLE_COUNT), random.unwrap_or(Self::DEFAULT_TIMESYNC_RANDOM))
            };

            let millis = period.as_millis();

            if millis < 10 || millis > (u32::MAX as u128) {
                panic!()
            }

            let millis = millis as u32;

            self.call_untyped("setTimesync", [millis.into(), sample_count.into(), random.into()].into_iter()).await
        }

        pub async fn set_system_information_logging_rate(&mut self, hz: f32) -> Result<rmpv::Value, rmpv::Value> {
            self.call_untyped("setSystemInformationLoggingRate", [hz.into()].into_iter()).await
        }

        pub async fn system_information_logging_rate(&mut self) -> Result<f32, String> {
            self.call("getSystemInformationLoggingRate", [].into_iter()).await
        }
        pub async fn is_eeprom_available(&mut self) -> Result<bool, String> {
            self.call("isEepromAvailable", [].into_iter()).await
        }

        pub async fn calibration(&mut self) -> Result<EepromData, String> {
            let res = self.call_untyped("getCalibration", [].into_iter()).await.map_err(|e| serde_rmpv::from_value::<String>(&e).unwrap())?;
            Ok(Self::parse_calibration(res))
        }

        pub async fn calibration2(&mut self) -> Result<EepromData, String> {
            let res = self.call_untyped("readFromEeprom", [].into_iter()).await.map_err(|e| serde_rmpv::from_value::<String>(&e).unwrap())?;
            Ok(Self::parse_calibration(res))
        }

        fn parse_calibration(val: rmpv::Value) -> EepromData {
            let rmpv::Value::Array(mut items) = val else {
                panic!();
            };

            let [_, _, data]: [rmpv::Value; 3] = items.try_into().unwrap();
            serde_rmpv::from_value(&data).unwrap()
        }

        pub async fn set_pipeline_schema(&mut self, schema: PipelineSchema) -> Result<rmpv::Value, rmpv::Value> {
            let val = serde_rmpv::to_value(&schema).unwrap();
            self.call_untyped("setPipelineSchema", [val].into_iter()).await
        }

        pub async fn wait_for_device_ready(&mut self) -> Result<rmpv::Value, rmpv::Value> {
            self.call_untyped("waitForDeviceReady", [].into_iter()).await
        }

        pub async fn build_pipeline(&mut self) -> Result<rmpv::Value, rmpv::Value> {
            self.call_untyped("buildPipeline", [].into_iter()).await
        }

        pub async fn start_pipeline(&mut self) -> Result<rmpv::Value, rmpv::Value> {
            self.call_untyped("startPipeline", [].into_iter()).await
        }
    }

    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, Hash, PartialEq, Eq, Default)]
    #[repr(i32)]
    pub enum CameraBoardSocket {
        #[default]
        Auto = -1,
        A = 0,
        B = 1,
        C = 2,
        D = 3, // also known as vertical
        E = 4,
        F = 5,
        G = 6,
        H = 7,
        I = 8,
        J = 9,
    }

    #[derive(serde_repr::Deserialize_repr, Debug)]
    #[repr(i32)]
    pub enum ConnectionInterface {
        Usb = 0,
        Ethernet = 1,
        Wifi = 2,
    }

    #[derive(serde::Deserialize, Debug)]
    pub struct CameraFeatures {
        socket: CameraBoardSocket,
        #[serde(rename = "sensorName")]
        sensor_name: String,
        width: i32,
        height: i32,
        orientation: CameraImageOrientation,
        #[serde(rename = "supportedTypes")]
        supported_types: Vec<CameraSensorType>,
        #[serde(rename = "hasAutofocusIC")]
        has_autofocus_ic: bool,
        #[serde(rename = "hasAutofocus")]
        has_autofocus: bool,
        name: String,
        #[serde(rename = "additionalNames")]
        additional_names: Vec<String>,
        configs: Vec<CameraSensorConfig>,
        #[serde(rename = "calibrationResolution")]
        calibration_resolution: Option<CameraSensorConfig>,
    }

    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, Default, PartialEq)]
    #[repr(i32)]
    pub enum CameraSensorType {
        #[default]
        Auto = -1,
        Color = 0,
        Mono = 1,
        Tof = 2,
        Thermal = 3,
    }

    #[derive(serde::Deserialize, Debug)]
    pub struct CameraSensorConfig {
        width: i32,
        height: i32,
        #[serde(rename = "minFps")]
        min_fps: f32,
        #[serde(rename = "maxFps")]
        max_fps: f32,
        fov: Rect,
        #[serde(rename = "type")]
        ty: CameraSensorType,
    }

    #[derive(serde::Deserialize, Debug)]
    pub struct Rect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        normalized: bool,
        #[serde(rename = "hasNormalized")]
        has_normalized: bool,
    }

    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, Default, PartialEq)]
    #[repr(i32)]
    pub enum CameraImageOrientation {
        #[default]
        Auto = -1,
        Normal = 0,
        HorizontalMirror = 1,
        VerticalFlip = 2,
        Rotate180 = 3,
    }

    #[derive(serde::Deserialize, Debug)]
    pub struct StereoPair {
        left: CameraBoardSocket,
        right: CameraBoardSocket,
        baseline: f32,
        #[serde(rename = "isVertical")]
        is_vertical: bool,
    }

    #[derive(serde::Deserialize, Debug)]
    pub struct MemoryInfo {
        remaining: i64,
        used: i64,
        total: i64,
    }

    #[derive(serde::Deserialize, Debug)]
    pub struct ChipTemperature {
        css: f32,
        mss: f32,
        upa: f32,
        dss: f32,
        average: f32,
    }

    #[derive(serde::Deserialize, Debug)]
    pub struct CpuUsage {
        average: f32,
        #[serde(rename = "msTime")]
        ms_time: i32,
    }

    #[derive(serde_repr::Deserialize_repr, Debug)]
    #[repr(u32)]
    pub enum UsbSpeed {
        Unknown = 0,
        Low = 1,
        Full = 2,
        High = 3,
        Super = 4,
        SuperPlus = 5,
    }

    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, Clone, Copy)]
    #[repr(u8)]
    pub enum LogLevel {
        Trace = 0,
        Debug = 1,
        Info = 2,
        Warn = 3,
        Error = 4,
        Critical = 5,
        Off = 6,
    }

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    pub struct EepromData {
        version: u32,
        #[serde(rename = "productName")]
        product_name: String,
        #[serde(rename = "boardCustom")]
        board_custom: String,
        #[serde(rename = "boardName")]
        board_name: String,
        #[serde(rename = "boardRev")]
        board_rev: String,
        #[serde(rename = "boardConf")]
        board_conf: String,
        #[serde(rename = "hardwareConf")]
        hardware_conf: String,
        #[serde(rename = "deviceName")]
        device_name: String,
        #[serde(rename = "batchName")]
        batch_name: Option<String>,
        #[serde(rename = "batchTime")]
        batch_time: u64,
        #[serde(rename = "boardOptions")]
        board_options: u32,
        #[serde(rename = "cameraData")]
        camera_data: Vec<(CameraBoardSocket, CameraInfo)>,
        #[serde(rename = "stereoRectificationData")]
        stereo_rectification: StereoRectification,
        #[serde(rename = "imuExtrinsics")]
        imu_extrinsics: Extrinsics,
        #[serde(rename = "housingExtrinsics")]
        housing_extrinsisc: Extrinsics,
        #[serde(rename = "miscellaneousData")]
        misc_data: Vec<u8>,
        #[serde(rename = "stereoUseSpecTranslation")]
        stereo_use_spec_translation: bool,
        #[serde(rename = "stereoEnableDistortionCorrection")]
        stereo_enable_distortion_correction: bool,
        #[serde(rename = "verticalCameraSocket")]
        vertical_camera_socket: CameraBoardSocket,
    }

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    pub struct StereoRectification {
        #[serde(rename = "rectifiedRotationLeft")]
        rectified_rotation_left: Vec<Vec<f32>>,
        #[serde(rename = "rectifiedRotationRight")]
        rectified_rotation_right: Vec<Vec<f32>>,
        #[serde(rename = "leftCameraSocket")]
        left_camera_socket: CameraBoardSocket,
        #[serde(rename = "rightCameraSocket")]
        right_camera_socket: CameraBoardSocket,
    }

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    pub struct CameraInfo {
        width: u16,
        height: u16,
        #[serde(rename = "lensPosition")]
        lens_position: u8,
        #[serde(rename = "intrinsicMatrix")]
        intrinsic_matrix: Vec<Vec<f32>>,
        #[serde(rename = "distortionCoeff")]
        distortion_coef: Vec<f32>,
        extrinsics: Extrinsics,
        #[serde(rename = "specHfovDeg")]
        fov_deg: f32,
        #[serde(rename = "cameraType")]
        ty: CameraModel,
    }

    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, PartialEq)]
    #[repr(i8)]
    pub enum CameraModel {
        Perspective = 0,
        Fisheye = 1,
        Equirectangular = 2,
        RadialDivision = 3,
    }

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    pub struct Extrinsics {
        #[serde(rename = "rotationMatrix")]
        rotation_mtx: Vec<Vec<f32>>,
        translation: Point3f,
        #[serde(rename = "specTranslation")]
        spec_translation: Point3f,
        #[serde(rename = "toCameraSocket")]
        to_camera_socket: CameraBoardSocket,
    }

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    pub struct Point3f {
        x: f32,
        y: f32,
        z: f32,
    }

    #[derive(serde::Deserialize, Debug)]
    pub struct CrashDump {
        #[serde(rename = "crashReports")]
        crash_reports: Vec<CrashReport>,
        #[serde(rename = "depthaiCommitHash")]
        dai_commit_hash: String,
        #[serde(rename = "deviceId")]
        device_id: String,
    }

    #[derive(serde::Deserialize, Debug)]
    pub struct CrashReport {
        processor: ProcessorType,
        #[serde(rename = "errorSource")]
        error_source: String,
        #[serde(rename = "crashedThreadId")]
        crashed_thread_id: u32,
        #[serde(rename = "errorSourceInfo")]
        error_source_info: ErrorSourceInfo,
        #[serde(rename = "threadCallstack")]
        thread_callstack: Vec<ThreadCallstack>,
        prints: Vec<String>,
        #[serde(rename = "uptimeNs")]
        uptime_ns: u64,
        #[serde(rename = "timerRaw")]
        timer_raw: u64,
        #[serde(rename = "statusFlags")]
        status_flags: u64,
    }

    #[derive(serde::Deserialize, Debug)]
    pub struct ErrorSourceInfo {
        #[serde(rename = "assertContext")]
        assert_context: AssertContext,
        #[serde(rename = "trapContext")]
        trap_context: TrapContext,
        #[serde(rename = "errorId")]
        error_id: u32,
    }

    #[derive(serde::Deserialize, Debug)]
    pub struct AssertContext {
        #[serde(rename = "fileName")]
        file_name: String,
        #[serde(rename = "functionName")]
        function_name: String,
        line: u32,
    }

    #[derive(serde::Deserialize, Debug)]
    pub struct TrapContext {
        #[serde(rename = "trapNumber")]
        trap_number: u32,
        #[serde(rename = "trapAddress")]
        trap_address: u32,
        #[serde(rename = "trapName")]
        trap_name: String,
    }

    #[derive(serde::Deserialize, Debug)]
    pub struct ThreadCallstack {
        #[serde(rename = "threadId")]
        thread_id: u32,
        #[serde(rename = "threadName")]
        thread_name: String,
        #[serde(rename = "threadStatus")]
        thread_status: String,
        #[serde(rename = "stackBottom")]
        stack_bottom: u32,
        #[serde(rename = "stackTop")]
        stack_top: u32,
        #[serde(rename = "stackPointer")]
        stack_pointer: u32,
        #[serde(rename = "instructionPointer")]
        instruction_pointer: u32,
        #[serde(rename = "callStack")]
        callstack: Vec<CallstackContext>,
    }

    #[derive(serde::Deserialize, Debug)]
    pub struct CallstackContext {
        #[serde(rename = "callSite")]
        callsite: u32,
        #[serde(rename = "calledTarget")]
        called_target: u32,
        #[serde(rename = "framePointer")]
        frame_pointer: u32,
        context: String,
    }

    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, PartialEq)]
    #[repr(i32)]
    pub enum ProcessorType {
        LeonCss = 0,
        LeonMss = 1,
        Cpu = 2,
        Dsp = 3,
    }

    // below is for starting pipeline stuff

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    pub struct PipelineSchema {
        pub connections: Vec<NodeConnectionSchema>,
        #[serde(rename = "globalProperties")]
        pub global_properties: GlobalProperties,
        pub nodes: Vec<(i64, NodeObjInfo)>,
        pub bridges: Vec<(i64, i64)>,
    }

    #[derive(serde::Deserialize, serde::Serialize, Debug, PartialEq, Eq)]
    pub struct NodeConnectionSchema {
        #[serde(rename = "node1Id")]
        pub output_id: i64,
        #[serde(rename = "node1OutputGroup")]
        pub output_group: String,
        #[serde(rename = "node1Output")]
        pub output: String,
        #[serde(rename = "node2Id")]
        pub input_id: i64,
        #[serde(rename = "node2InputGroup")]
        pub input_group: String,
        #[serde(rename = "node2Input")]
        pub input: String,
    }

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    pub struct GlobalProperties {
        #[serde(rename = "leonCssFrequencyHz")]
        pub leon_css_frequency_hz: f32,
        #[serde(rename = "leonMssFrequencyHz")]
        pub leon_mss_frequency_hz: f32,
        #[serde(rename = "pipelineName")]
        pub pipeline_name: Option<String>,
        #[serde(rename = "pipelineVersion")]
        pub pipeline_version: Option<String>,
        #[serde(rename = "calibData")]
        pub calibration: Option<EepromData>,
        #[serde(rename = "eepromId")]
        pub eeprom_id: Option<u32>,
        #[serde(rename = "cameraTuningBlobSize")]
        pub camera_tuning_blob_size: Option<u32>,
        #[serde(rename = "cameraTuningBlobUri")]
        pub camera_tuning_blob_uri: String,
        #[serde(rename = "cameraSocketTuningBlobSize")]
        pub camera_socket_tuning_blob_size: Vec<(CameraBoardSocket, u32)>,
        #[serde(rename = "cameraSocketTuningBlobUri")]
        pub camera_socket_tuning_blob_uri: Vec<(CameraBoardSocket, String)>,
        #[serde(rename = "xlinkChunkSize")]
        pub xlink_chunk_size: i32,
        #[serde(rename = "sippBufferSize")]
        pub sipp_buffer_size: u32,
        #[serde(rename = "sippDmaBufferSize")]
        pub sipp_dma_buffer_size: u32,
    }

    impl core::default::Default for GlobalProperties {
        fn default() -> Self {
            Self {
                leon_css_frequency_hz: 700. * 1000. * 1000.,
                leon_mss_frequency_hz: 700. * 1000. * 1000.,
                pipeline_name: None,
                pipeline_version: None,
                calibration: None,
                eeprom_id: Some(0),
                camera_tuning_blob_size: None,
                camera_tuning_blob_uri: String::new(),
                camera_socket_tuning_blob_size: Default::default(),
                camera_socket_tuning_blob_uri: Default::default(),
                xlink_chunk_size: -1,
                sipp_buffer_size: Self::SIPP_BUFFER_DEFAULT_SIZE,
                sipp_dma_buffer_size: Self::SIPP_DMA_BUFFER_DEFAULT_SIZE,
            }
        }
    }

    impl GlobalProperties {
        pub const SIPP_BUFFER_DEFAULT_SIZE: u32 = 18 * 1024;
        pub const SIPP_DMA_BUFFER_DEFAULT_SIZE: u32 = 16 * 1024;
    }

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    pub struct NodeObjInfo {
        pub id: i64,
        #[serde(rename = "parentId")]
        pub parent_id: i64,
        pub name: String,
        pub alias: String,
        #[serde(rename = "deviceId")]
        pub device_id: String,
        #[serde(rename = "deviceNode")]
        pub device_node: bool,
        pub properties: Vec<u8>,
        #[serde(rename = "logLevel")]
        pub log_level: LogLevel,
        #[serde(rename = "ioInfo")]
        pub io_info: Vec<((String, String), NodeIoInfo)>,
    }

    #[derive(serde::Deserialize, serde::Serialize, Debug)]
    pub struct NodeIoInfo {
        pub group: String,
        pub name: String,
        #[serde(rename = "type")]
        pub ty: NodeType,
        pub blocking: bool,
        #[serde(rename = "queueSize")]
        pub queue_size: i32,
        #[serde(rename = "waitForMessage")]
        pub wait_for_message: bool,
        pub id: u32,
    }

    #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug)]
    #[repr(u8)]
    pub enum NodeType {
        MSender = 0,
        SSender = 1,
        MReceiver = 2,
        SReceiver = 3,
    }
    // end of pipeline
}
