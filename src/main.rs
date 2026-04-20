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


        struct Rpc<'a> {
            inner: &'a mut ConnectionStream,
        }

        
        impl <'a> Rpc<'a> {
            fn new(stream: &'a mut ConnectionStream) -> Self {
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

            async fn is_running(&mut self) -> bool {
                self.call::<_, String>("isRunning", [].into_iter()).await.unwrap()
            }

            async fn enable_crash_dump(&mut self, enable: bool) -> Result<(), String>{
                self.call_untyped("enableCrashDump", [enable.into()].into_iter()).await.map_err(|e| serde_rmpv::from_value::<String>(&e).unwrap())?;
                Ok(())
            }

            async fn mxid(&mut self) -> Result<String, String> {
                self.call("getMxId", [].into_iter()).await
            }

            async fn connected_cameras(&mut self) -> Result<Vec<CameraBoardSocket>, String> {
                self.call("getConnectedCameras", [].into_iter()).await
            }

            async fn connection_interfaces(&mut self) -> Result<Vec<ConnectionInterface>, String> {
                self.call("getConnectionInterfaces", [].into_iter()).await
            }

            async fn connected_camera_features(&mut self) -> Result<Vec<CameraFeatures>, String> {
                self.call("getConnectedCameraFeatures", [].into_iter()).await
            }
            /*
            async fn connected_camera_features(&mut self) -> Result<rmpv::Value, rmpv::Value> {
                self.call_untyped("getConnectedCameraFeatures", [].into_iter()).await
            }
            */

            async fn stereo_pairs(&mut self) -> Result<Vec<StereoPair>, String> {
                self.call("getStereoPairs", [].into_iter()).await
            }

            async fn camera_sensor_names(&mut self) -> Result<HashMap<CameraBoardSocket, String>, String> {
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

            async fn connected_imu(&mut self) -> Result<String, String> {
                self.call("getConnectedIMU", [].into_iter()).await
            }

            async fn crash_device(&mut self) -> Result<rmpv::Value, rmpv::Value> {
                self.call_untyped("crashDevice", [].into_iter()).await
            }

            async fn external_strobe_enable(&mut self, enable: bool) -> Result<rmpv::Value, rmpv::Value> {
                self.call_untyped("setExternalStrobeEnable", [enable.into()].into_iter()).await
            }

            async fn imu_firmware_version(&mut self) -> Result<String, String> {
                self.call("getIMUFirmwareVersion", [].into_iter()).await
            }

            async fn embedded_imu_firmware_version(&mut self) -> Result<String, String> {
                self.call("getEmbeddedIMUFirmwareVersion", [].into_iter()).await
            }

            async fn ddr_memory_usage(&mut self) -> Result<MemoryInfo, String> {
                self.call("getDdrUsage", [].into_iter()).await
            }
            
            async fn cmx_memory_usage(&mut self) -> Result<MemoryInfo, String> {
                self.call("getCmxUsage", [].into_iter()).await
            }

            async fn leon_css_heap_usage(&mut self) -> Result<MemoryInfo, String> {
                self.call("getLeonCssHeapUsage", [].into_iter()).await
            }

            async fn leon_mss_heap_usage(&mut self) -> Result<MemoryInfo, String> {
                self.call("getLeonMssHeapUsage", [].into_iter()).await
            }

            async fn chip_temperature(&mut self) -> Result<ChipTemperature, String> {
                self.call("getChipTemperature", [].into_iter()).await
            }

            async fn leon_css_cpu_usage(&mut self) -> Result<CpuUsage, String> {
                self.call("getLeonCssCpuUsage", [].into_iter()).await
            }

            async fn leon_mss_cpu_usage(&mut self) -> Result<CpuUsage, String> {
                self.call("getLeonMssCpuUsage", [].into_iter()).await
            }

            async fn process_memory_usage(&mut self) -> Result<i64, String> {
                self.call("getProcessMemoryUsage", [].into_iter()).await
            }

            async fn usb_speed(&mut self) -> Result<UsbSpeed, String> {
                self.call("getUsbSpeed", [].into_iter()).await
            }

            async fn is_neural_depth_supported(&mut self) -> Result<bool, String> {
                self.call("isNeuralDepthSupported", [].into_iter()).await
            }

            async fn is_pipeline_running(&mut self) -> Result<bool, String> {
                self.call("isPipelineRunning", [].into_iter()).await
            }

            async fn set_log_level(&mut self, log_level: LogLevel) -> Result<rmpv::Value, rmpv::Value> {
                self.call_untyped("setLogLevel", [(log_level as u8).into()].into_iter()).await
            }

            async fn set_node_log_level(&mut self, node_id: i64, log_level: LogLevel) -> Result<rmpv::Value, rmpv::Value> {
                self.call_untyped("setNodeLogLevel", [node_id.into(), (log_level as u8).into()].into_iter()).await
            }

            async fn log_level(&mut self) -> Result<u32, String> {
                self.call("getLogLevel", [].into_iter()).await
            }

            async fn node_log_level(&mut self, node_id: u64) -> Result<u32, String> {
                self.call("getNodeLogLevel", [node_id.into()].into_iter()).await
            }

            async fn xlink_chunk_size(&mut self) -> Result<i32, String> {
                self.call("getXLinkChunkSize", [].into_iter()).await
            }

            async fn set_ir_laser_dot_projector_intensity(&mut self, intensity: f32, mask: i32) -> Result<i32, String> {
                self.call("setIrLaserDotProjectorBrightness", [intensity.into(), mask.into(), true.into()].into_iter()).await
            }

            async fn set_ir_floodlight_intensity(&mut self, intensity: f32, mask: i32) -> Result<i32, String> {
                self.call("setIrFloodLightBrightness", [intensity.into(), mask.into(), true.into()].into_iter()).await
            }

            async fn ir_drivers(&mut self) -> Result<Option<(String, i32, i32)>, String> {
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
            async fn crash_dump(&mut self, clear: bool) -> Result<CrashDump, String> {
                self.call("getCrashDump", [clear.into()].into_iter()).await
            }

            async fn has_crash_dump(&mut self) -> Result<bool, String> {
                self.call("hasCrashDump", [].into_iter()).await
            }

            const DEFAULT_TIMESYNC_PERIOD: std::time::Duration = std::time::Duration::from_millis(1000);
            const DEFAULT_TIMESYNC_SAMPLE_COUNT: u32 = 1000;
            const DEFAULT_TIMESYNC_RANDOM: bool = false;
            async fn set_timesync(&mut self, period: Option<std::time::Duration>, sample_count: Option<u32>, random: Option<bool>, enable: bool) -> Result<rmpv::Value, rmpv::Value> {
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

            async fn set_system_information_logging_rate(&mut self, hz: f32) -> Result<rmpv::Value, rmpv::Value> {
                self.call_untyped("setSystemInformationLoggingRate", [hz.into()].into_iter()).await
            }

            async fn system_information_logging_rate(&mut self) -> Result<f32, String> {
                self.call("getSystemInformationLoggingRate", [].into_iter()).await
            }
            async fn is_eeprom_available(&mut self) -> Result<bool, String> {
                self.call("isEepromAvailable", [].into_iter()).await
            }

            async fn calibration(&mut self) -> Result<EepromData, String> {
                let res = self.call_untyped("getCalibration", [].into_iter()).await.map_err(|e| serde_rmpv::from_value::<String>(&e).unwrap())?;
                Ok(Self::parse_calibration(res))
            }

            async fn calibration2(&mut self) -> Result<EepromData, String> {
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

            async fn set_pipeline_schema(&mut self, schema: PipelineSchema) -> Result<rmpv::Value, rmpv::Value> {
                let val = serde_rmpv::to_value(&schema).unwrap();
                self.call_untyped("setPipelineSchema", [val].into_iter()).await
            }

            async fn wait_for_device_ready(&mut self) -> Result<rmpv::Value, rmpv::Value> {
                self.call_untyped("waitForDeviceReady", [].into_iter()).await
            }

            async fn build_pipeline(&mut self) -> Result<rmpv::Value, rmpv::Value> {
                self.call_untyped("buildPipeline", [].into_iter()).await
            }

            async fn start_pipeline(&mut self) -> Result<rmpv::Value, rmpv::Value> {
                self.call_untyped("startPipeline", [].into_iter()).await
            }

            //TODO: pipeline stuff (just more rpc calls)
        }

        #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug, Hash, PartialEq, Eq)]
        #[repr(i32)]
        enum CameraBoardSocket {
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
        enum ConnectionInterface {
            Usb = 0,
            Ethernet = 1,
            Wifi = 2,
        }

        #[derive(serde::Deserialize, Debug)]
        struct CameraFeatures {
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

        #[derive(serde_repr::Deserialize_repr, Debug)]
        #[repr(i32)]
        enum CameraSensorType {
            Auto = -1,
            Color = 0,
            Mono = 1,
            Tof = 2,
            Thermal = 3,
        }

        #[derive(serde::Deserialize, Debug)]
        struct CameraSensorConfig {
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
        struct Rect {
            x: f32,
            y: f32,
            width: f32,
            height: f32,
            normalized: bool,
            #[serde(rename = "hasNormalized")]
            has_normalized: bool,
        }

        #[derive(serde_repr::Deserialize_repr, Debug)]
        #[repr(i32)]
        enum CameraImageOrientation {
            Auto = -1,
            Normal = 0,
            HorizontalMirror = 1,
            VerticalFlip = 2,
            Rotate180 = 3,
        }

        #[derive(serde::Deserialize, Debug)]
        struct StereoPair {
            left: CameraBoardSocket,
            right: CameraBoardSocket,
            baseline: f32,
            #[serde(rename = "isVertical")]
            is_vertical: bool,
        }

        #[derive(serde::Deserialize, Debug)]
        struct MemoryInfo {
            remaining: i64,
            used: i64,
            total: i64,
        }

        #[derive(serde::Deserialize, Debug)]
        struct ChipTemperature {
            css: f32,
            mss: f32,
            upa: f32,
            dss: f32,
            average: f32,
        }

        #[derive(serde::Deserialize, Debug)]
        struct CpuUsage {
            average: f32,
            #[serde(rename = "msTime")]
            ms_time: i32,
        }

        #[derive(serde_repr::Deserialize_repr, Debug)]
        #[repr(u32)]
        enum UsbSpeed {
            Unknown = 0,
            Low = 1,
            Full = 2,
            High = 3,
            Super = 4,
            SuperPlus = 5,
        }

        #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug)]
        #[repr(u8)]
        enum LogLevel {
            Trace = 0,
            Debug = 1,
            Info = 2,
            Warn = 3,
            Error = 4,
            Critical = 5,
            Off = 6,
        }

        #[derive(serde::Deserialize, serde::Serialize, Debug)]
        struct EepromData {
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
        struct StereoRectification {
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
        struct CameraInfo {
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

        #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug)]
        #[repr(i8)]
        enum CameraModel {
            Perspective = 0,
            Fisheye = 1,
            Equirectangular = 2,
            RadialDivision = 3,
        }

        #[derive(serde::Deserialize, serde::Serialize, Debug)]
        struct Extrinsics {
            #[serde(rename = "rotationMatrix")]
            rotation_mtx: Vec<Vec<f32>>,
            translation: Point3f,
            #[serde(rename = "specTranslation")]
            spec_translation: Point3f,
            #[serde(rename = "toCameraSocket")]
            to_camera_socket: CameraBoardSocket,
        }

        #[derive(serde::Deserialize, serde::Serialize, Debug)]
        struct Point3f {
            x: f32,
            y: f32,
            z: f32,
        }

        #[derive(serde::Deserialize, Debug)]
        struct CrashDump {
            #[serde(rename = "crashReports")]
            crash_reports: Vec<CrashReport>,
            #[serde(rename = "depthaiCommitHash")]
            dai_commit_hash: String,
            #[serde(rename = "deviceId")]
            device_id: String,
        }

        #[derive(serde::Deserialize, Debug)]
        struct CrashReport {
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
        struct ErrorSourceInfo {
            #[serde(rename = "assertContext")]
            assert_context: AssertContext,
            #[serde(rename = "trapContext")]
            trap_context: TrapContext,
            #[serde(rename = "errorId")]
            error_id: u32,
        }

        #[derive(serde::Deserialize, Debug)]
        struct AssertContext {
            #[serde(rename = "fileName")]
            file_name: String,
            #[serde(rename = "functionName")]
            function_name: String,
            line: u32,
        }

        #[derive(serde::Deserialize, Debug)]
        struct TrapContext {
            #[serde(rename = "trapNumber")]
            trap_number: u32,
            #[serde(rename = "trapAddress")]
            trap_address: u32,
            #[serde(rename = "trapName")]
            trap_name: String,
        }

        #[derive(serde::Deserialize, Debug)]
        struct ThreadCallstack {
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
        struct CallstackContext {
            #[serde(rename = "callSite")]
            callsite: u32,
            #[serde(rename = "calledTarget")]
            called_target: u32,
            #[serde(rename = "framePointer")]
            frame_pointer: u32,
            context: String,
        }

        #[derive(serde_repr::Deserialize_repr, Debug)]
        #[repr(i32)]
        enum ProcessorType {
            LeonCss = 0,
            LeonMss = 1,
            Cpu = 2,
            Dsp = 3,
        }

        // below is for starting pipeline stuff

        #[derive(serde::Deserialize, serde::Serialize, Debug)]
        struct PipelineSchema {
            connections: Vec<NodeConnectionSchema>,
            #[serde(rename = "globalProperties")]
            global_properties: GlobalProperties,
            nodes: Vec<(i64, NodeObjInfo)>,
            bridges: Vec<(i64, i64)>,
        }

        #[derive(serde::Deserialize, serde::Serialize, Debug, PartialEq, Eq)]
        struct NodeConnectionSchema {
            #[serde(rename = "node1Id")]
            output_id: i64,
            #[serde(rename = "node1OutputGroup")]
            output_group: String,
            #[serde(rename = "node1Output")]
            output: String,
            #[serde(rename = "node2Id")]
            input_id: i64,
            #[serde(rename = "node2InputGroup")]
            input_group: String,
            #[serde(rename = "node2Input")]
            input: String,
        }

        #[derive(serde::Deserialize, serde::Serialize, Debug)]
        struct GlobalProperties {
            #[serde(rename = "leonCssFrequencyHz")]
            leon_css_frequency_hz: f32,
            #[serde(rename = "leonMssFrequencyHz")]
            leon_mss_frequency_hz: f32,
            #[serde(rename = "pipelineName")]
            pipeline_name: Option<String>,
            #[serde(rename = "pipelineVersion")]
            pipeline_version: Option<String>,
            #[serde(rename = "calibData")]
            calibration: Option<EepromData>,
            #[serde(rename = "eepromId")]
            eeprom_id: Option<u32>,
            #[serde(rename = "cameraTuningBlobSize")]
            camera_tuning_blob_size: Option<u32>,
            #[serde(rename = "cameraTuningBlobUri")]
            camera_tuning_blob_uri: String,
            #[serde(rename = "cameraSocketTuningBlobSize")]
            camera_socket_tuning_blob_size: Vec<(CameraBoardSocket, u32)>,
            #[serde(rename = "cameraSocketTuningBlobUri")]
            camera_socket_tuning_blob_uri: Vec<(CameraBoardSocket, String)>,
            #[serde(rename = "xlinkChunkSize")]
            xlink_chunk_size: i32,
            #[serde(rename = "sippBufferSize")]
            sipp_buffer_size: u32,
            #[serde(rename = "sippDmaBufferSize")]
            sipp_dma_buffer_size: u32,
        }

        impl GlobalProperties {
            const SIPP_BUFFER_DEFAULT_SIZE: u32 = 18 * 1024;
            const SIPP_DMA_BUFFER_DEFAULT_SIZE: u32 = 16 * 1024;
        }

        #[derive(serde::Deserialize, serde::Serialize, Debug)]
        struct NodeObjInfo {
            id: i64,
            #[serde(rename = "parentId")]
            parent_id: i64,
            name: String,
            alias: String,
            #[serde(rename = "deviceId")]
            device_id: String,
            #[serde(rename = "deviceNode")]
            device_node: bool,
            properties: Vec<u8>,
            #[serde(rename = "logLevel")]
            log_level: LogLevel,
            #[serde(rename = "ioInfo")]
            io_info: Vec<((String, String), NodeIoInfo)>,
        }

        #[derive(serde::Deserialize, serde::Serialize, Debug)]
        struct NodeIoInfo {
            group: String,
            name: String,
            #[serde(rename = "type")]
            ty: NodeType,
            blocking: bool,
            #[serde(rename = "queueSize")]
            queue_size: i32,
            #[serde(rename = "waitForMessage")]
            wait_for_message: bool,
            id: u32,
        }

        #[derive(serde_repr::Deserialize_repr, serde_repr::Serialize_repr, Debug)]
        #[repr(u8)]
        enum NodeType {
            MSender = 0,
            SSender = 1,
            MReceiver = 2,
            SReceiver = 3,
        }
        // end of pipeline


        // libnop encoded structs

        #[derive(serde::Deserialize, Debug)]
        struct SystemInfo {
            ddr_memory_usage: MemoryInfo,
            cmx_memory_usage: MemoryInfo,
            leon_css_memory_usage: MemoryInfo,
            leon_mss_memory_usage: MemoryInfo,
            leon_css_cpu_usage: CpuUsage,
            leon_mss_cpu_usage: CpuUsage,
            chip_temperature: ChipTemperature,
        }

        //end of libnop type defines
        
        let mut rpc = Rpc::new(&mut rpc_stream);

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

        let features = rpc.connected_camera_features().await.unwrap();
        println!("features: {features:?}");

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

        let schema = PipelineSchema {
            bridges: vec![],
            connections: vec![
                NodeConnectionSchema {
                    output_id: 0,
                    output: "out".into(),
                    output_group: "".into(),
                    input_id: 1,
                    input: "in".into(),
                    input_group: "".into(),
                },
            ],
            global_properties: GlobalProperties {
                calibration: None,
                camera_socket_tuning_blob_size: vec![],
                camera_socket_tuning_blob_uri: vec![],
                camera_tuning_blob_size: None,
                camera_tuning_blob_uri: "".into(),
                eeprom_id: Some(0),
                leon_css_frequency_hz: 700000000.,
                leon_mss_frequency_hz: 700000000.,
                pipeline_name: None,
                pipeline_version: None,
                sipp_buffer_size: GlobalProperties::SIPP_BUFFER_DEFAULT_SIZE,
                sipp_dma_buffer_size: GlobalProperties::SIPP_DMA_BUFFER_DEFAULT_SIZE,
                xlink_chunk_size: -1,
            },
            nodes: vec![
                (1, NodeObjInfo {
                    alias: "".into(),
                    device_id: "19443010A1A1872D00".into(),
                    device_node: true,
                    id: 1,
                    io_info: vec![
                        (("".into(), "pipelineEventOutput".into()), NodeIoInfo {
                            blocking: false, 
                            group: "".into(),
                            id: 3,
                            name: "pipelineEventOutput".into(),
                            queue_size: 8,
                            ty: NodeType::MSender,
                            wait_for_message: false,
                        }), 
                        (("".into(), "in".into()), NodeIoInfo {
                            blocking: true,
                            group: "".into(),
                            id: 2,
                            name: "in".into(),
                            queue_size: 3,
                            ty: NodeType::SReceiver,
                            wait_for_message: false,
                        }),
                    ],
                    log_level: LogLevel::Trace,
                    name: "XLinkOut".into(),
                    parent_id: -1,
                    properties: vec![185, 5, 136, 0, 0, 128, 191, 189, 9, 95, 95, 120, 95, 48, 95, 111, 117, 116, 0, 255, 255],
                }),
                (0, NodeObjInfo {
                    alias: "".into(),
                    device_id: "19443010A1A1872D00".into(),
                    device_node: true,
                    id: 0,
                    io_info: vec![
                        (("".into(), "out".into()), NodeIoInfo {
                            blocking: false,
                            group: "".into(),
                            id: 1,
                            name: "out".into(),
                            queue_size: 8,
                            ty: NodeType::MSender,
                            wait_for_message: false,
                        }),
                        (("".into(), "pipelineEventOutput".into()), NodeIoInfo {
                            blocking: false,
                            group: "".into(),
                            id: 0,
                            name: "pipelineEventOutput".into(),
                            queue_size: 8,
                            ty: NodeType::MSender,
                            wait_for_message: false,
                        }),
                    ],
                    log_level: LogLevel::Trace,
                    name: "SystemLogger".into(),
                    parent_id: -1,
                    properties: vec![185, 1, 136, 0, 0, 128, 63],
                })
            ]
        };

        connection.create_stream("__x_0_out", bootloader::MAX_PACKET_SIZE).await.unwrap();

        let ret = rpc.set_pipeline_schema(schema).await;
        println!("{ret:?}");

        let ret = rpc.wait_for_device_ready().await;
        println!("{ret:?}");

        let ret = rpc.build_pipeline().await;
        println!("{ret:?}");

        let ret = rpc.start_pipeline().await;
        println!("{ret:?}");

        connection.wait_for_stream("__x_0_out").await;

        let out_name = StreamName::new(&"__x_0_out".as_bytes()).unwrap();

        let mut output = connection.created_streams.remove(&out_name).unwrap();
        println!("xout: {output:?}");

        loop {
            let mut bytes = output.read().await;

            let val = rnop::Value::parse(&bytes).unwrap();
            let val = rnop::from_value::<SystemInfo>(val).unwrap();
            println!("\n{val:?}");
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
        /*
        let ev = Event::read_release(self.read.id, b"", bytes.len() as _);

        // the write id / read release id in the header need to be the same i think
        //  - not the stream_id, but the actual id.



        self.writer.send(StreamEvent::ReadRelease(ev.header)).await.unwrap();
        */
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
                IoEvent::CreatedStream(created_name, stream) => {
                    self.created_streams.insert(created_name, stream);
                    if created_name.as_bytes() == name.as_bytes() {
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
    CreatedStream(StreamName, ConnectionStream),
    DeviceRead(u32, Vec<u8>),
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
        let mut readable = (&mut stream).take(HEADER_LEN as _);

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

                        if let Some(IoStream { writer_fb, .. }) = created_streams.get_mut(&header.stream_id) {
                            writer_fb.notify_one();
                        } else {
                            panic!("could not find stream for {}", header.stream_id);
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
                    EventType::WriteResp | EventType::ReadResp | EventType::ReadRelResp | EventType::ReadRelSpecResp | EventType::CloseStreamResp => continue,

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
