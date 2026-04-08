#[cfg(windows)]
mod imp {
    use std::mem::ManuallyDrop;
    use std::ptr::null_mut;
    use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
    use std::thread::{self, JoinHandle};

    use windows::Win32::Media::MediaFoundation::{
        CLSID_MSH264EncoderMFT, MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT,
        MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
        MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
        MF_VERSION, MFCreateMediaType, MFCreateMemoryBuffer,
        MFCreateSample, MFMediaType_Video, MFSTARTUP_FULL, MFShutdown, MFStartup,
        MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFT_MESSAGE_COMMAND_DRAIN,
        MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
        MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_INFO,
        MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_SET_TYPE_TEST_ONLY, IMFMediaBuffer, IMFMediaType,
        IMFSample, IMFTransform,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };

    pub struct EncodedAccessUnit {
        pub data: Vec<u8>,
        pub keyframe: bool,
    }

    impl Clone for EncodedAccessUnit {
        fn clone(&self) -> Self {
            Self {
                data: self.data.clone(),
                keyframe: self.keyframe,
            }
        }
    }

    enum WorkerCommand {
        Encode {
            width: usize,
            height: usize,
            nv12: Vec<u8>,
        },
        Shutdown,
    }

    enum WorkerResponse {
        Encoded(Vec<EncodedAccessUnit>),
        Error(String),
    }

    pub struct MediaFoundationEncoderWorker {
        command_tx: SyncSender<WorkerCommand>,
        response_rx: Receiver<WorkerResponse>,
        thread_handle: Option<JoinHandle<()>>,
    }

    impl MediaFoundationEncoderWorker {
        pub fn new(initial_width: usize, initial_height: usize, target_fps: u32, bitrate_bps: u32) -> Result<Self, String> {
            let (command_tx, command_rx) = sync_channel::<WorkerCommand>(2);
            let (response_tx, response_rx) = sync_channel::<WorkerResponse>(2);

            let thread_handle = thread::Builder::new()
                .name("mf-h264-encoder".to_string())
                .spawn(move || {
                    let mut active_dimensions: Option<(usize, usize)> = None;
                    let mut encoder: Option<MediaFoundationH264Encoder> = None;

                    if initial_width >= 2 && initial_height >= 2 {
                        match MediaFoundationH264Encoder::new(
                            initial_width,
                            initial_height,
                            target_fps,
                            bitrate_bps,
                        ) {
                            Ok(created) => {
                                active_dimensions = Some((initial_width, initial_height));
                                encoder = Some(created);
                            }
                            Err(err) => {
                                let _ = response_tx.send(WorkerResponse::Error(err));
                            }
                        }
                    }

                    while let Ok(command) = command_rx.recv() {
                        match command {
                            WorkerCommand::Shutdown => break,
                            WorkerCommand::Encode { width, height, nv12 } => {
                                if active_dimensions != Some((width, height)) {
                                    match MediaFoundationH264Encoder::new(
                                        width,
                                        height,
                                        target_fps,
                                        bitrate_bps,
                                    ) {
                                        Ok(created) => {
                                            active_dimensions = Some((width, height));
                                            encoder = Some(created);
                                        }
                                        Err(err) => {
                                            let _ = response_tx.send(WorkerResponse::Error(err));
                                            continue;
                                        }
                                    }
                                }

                                let Some(active_encoder) = encoder.as_mut() else {
                                    let _ = response_tx.send(WorkerResponse::Error(
                                        "Media Foundation encoder is not initialized".to_string(),
                                    ));
                                    continue;
                                };

                                match active_encoder.encode_nv12(width, height, &nv12) {
                                    Ok(encoded) => {
                                        let _ = response_tx.send(WorkerResponse::Encoded(encoded));
                                    }
                                    Err(err) => {
                                        let _ = response_tx.send(WorkerResponse::Error(err));
                                    }
                                }
                            }
                        }
                    }
                })
                .map_err(|err| format!("Failed to spawn Media Foundation worker thread: {err}"))?;

            Ok(Self {
                command_tx,
                response_rx,
                thread_handle: Some(thread_handle),
            })
        }

        pub fn encode_nv12(
            &mut self,
            width: usize,
            height: usize,
            nv12_bytes: Vec<u8>,
        ) -> Result<Vec<EncodedAccessUnit>, String> {
            self.command_tx
                .send(WorkerCommand::Encode {
                    width,
                    height,
                    nv12: nv12_bytes,
                })
                .map_err(|err| format!("Media Foundation worker send failed: {err}"))?;

            match self
                .response_rx
                .recv()
                .map_err(|err| format!("Media Foundation worker recv failed: {err}"))?
            {
                WorkerResponse::Encoded(data) => Ok(data),
                WorkerResponse::Error(err) => Err(err),
            }
        }
    }

    impl Drop for MediaFoundationEncoderWorker {
        fn drop(&mut self) {
            let _ = self.command_tx.send(WorkerCommand::Shutdown);
            if let Some(handle) = self.thread_handle.take() {
                let _ = handle.join();
            }
        }
    }

    pub struct MediaFoundationH264Encoder {
        transform: IMFTransform,
        output_stream_id: u32,
        output_sample_provided_by_mft: bool,
        output_buffer_size: u32,
        frame_index: i64,
        frame_duration_hns: i64,
    }

    impl MediaFoundationH264Encoder {
        pub fn new(width: usize, height: usize, target_fps: u32, bitrate_bps: u32) -> Result<Self, String> {
            if width < 2 || height < 2 || width % 2 != 0 || height % 2 != 0 {
                return Err("Media Foundation encoder requires even frame dimensions".to_string());
            }
            if target_fps == 0 {
                return Err("Media Foundation encoder requires target_fps > 0".to_string());
            }

            unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED)
                    .ok()
                    .map_err(|err| format!("CoInitializeEx failed: {err}"))?;
                MFStartup(MF_VERSION, MFSTARTUP_FULL)
                    .map_err(|err| format!("MFStartup failed: {err}"))?;

                let transform: IMFTransform = CoCreateInstance(
                    &CLSID_MSH264EncoderMFT,
                    None,
                    CLSCTX_INPROC_SERVER,
                )
                .map_err(|err| format!("CoCreateInstance(CMSH264EncoderMFT) failed: {err}"))?;

                configure_media_types(&transform, width as u32, height as u32, target_fps, bitrate_bps)?;

                let mut input_stream_count = 0u32;
                let mut output_stream_count = 0u32;
                transform
                    .GetStreamCount(
                        &mut input_stream_count as *mut u32,
                        &mut output_stream_count as *mut u32,
                    )
                    .map_err(|err| format!("GetStreamCount failed: {err}"))?;
                if input_stream_count == 0 || output_stream_count == 0 {
                    return Err("H264 MFT exposes no output stream".to_string());
                }
                let output_stream_id = 0u32;

                let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
                let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);

                let output_info: MFT_OUTPUT_STREAM_INFO = transform
                    .GetOutputStreamInfo(output_stream_id)
                    .map_err(|err| format!("GetOutputStreamInfo failed: {err}"))?;

                Ok(Self {
                    transform,
                    output_stream_id,
                    output_sample_provided_by_mft: (output_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0,
                    output_buffer_size: output_info.cbSize.max(64 * 1024),
                    frame_index: 0,
                    frame_duration_hns: (10_000_000i64 / target_fps as i64).max(1),
                })
            }
        }

        pub fn encode_nv12(
            &mut self,
            width: usize,
            height: usize,
            nv12_bytes: &[u8],
        ) -> Result<Vec<EncodedAccessUnit>, String> {
            let expected = width
                .checked_mul(height)
                .and_then(|y| y.checked_add((width * height) / 2))
                .ok_or_else(|| "Invalid NV12 frame size".to_string())?;
            if nv12_bytes.len() != expected {
                return Err(format!(
                    "Invalid NV12 payload size: got {}, expected {}",
                    nv12_bytes.len(),
                    expected
                ));
            }

            unsafe {
                let sample = build_input_sample(
                    nv12_bytes,
                    self.frame_index * self.frame_duration_hns,
                    self.frame_duration_hns,
                )?;

                match self.transform.ProcessInput(0, &sample, 0) {
                    Ok(()) => {
                        self.frame_index += 1;
                        drain_outputs_until_need_more_input(
                            &self.transform,
                            self.output_stream_id,
                            self.output_sample_provided_by_mft,
                            self.output_buffer_size,
                        )
                    }
                    Err(err) if err.code() == MF_E_NOTACCEPTING => Ok(vec![]),
                    Err(err) => Err(format!("ProcessInput failed: {err}")),
                }
            }
        }
    }

    impl Drop for MediaFoundationH264Encoder {
        fn drop(&mut self) {
            unsafe {
                let _ = self
                    .transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
                let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
                let _ = MFShutdown();
                CoUninitialize();
            }
        }
    }

    unsafe fn configure_media_types(
        transform: &IMFTransform,
        width: u32,
        height: u32,
        target_fps: u32,
        bitrate_bps: u32,
    ) -> Result<(), String> {
        let output_type: IMFMediaType =
            MFCreateMediaType().map_err(|err| format!("MFCreateMediaType(output) failed: {err}"))?;
        output_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|err| format!("Set output major type failed: {err}"))?;
        output_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
            .map_err(|err| format!("Set output subtype failed: {err}"))?;
        output_type
            .SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | (height as u64))
            .map_err(|err| format!("Set output frame size failed: {err}"))?;
        output_type
            .SetUINT64(&MF_MT_FRAME_RATE, ((target_fps as u64) << 32) | 1)
            .map_err(|err| format!("Set output frame rate failed: {err}"))?;
        output_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)
            .map_err(|err| format!("Set output pixel aspect ratio failed: {err}"))?;
        output_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|err| format!("Set output interlace mode failed: {err}"))?;
        output_type
            .SetUINT32(&MF_MT_AVG_BITRATE, bitrate_bps)
            .map_err(|err| format!("Set output bitrate failed: {err}"))?;

        transform
            .SetOutputType(0, &output_type, MFT_SET_TYPE_TEST_ONLY.0 as u32)
            .map_err(|err| format!("SetOutputType(TEST_ONLY) failed: {err}"))?;
        transform
            .SetOutputType(0, &output_type, 0)
            .map_err(|err| format!("SetOutputType failed: {err}"))?;

        let input_type: IMFMediaType =
            MFCreateMediaType().map_err(|err| format!("MFCreateMediaType(input) failed: {err}"))?;
        input_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|err| format!("Set input major type failed: {err}"))?;
        input_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
            .map_err(|err| format!("Set input subtype failed: {err}"))?;
        input_type
            .SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | (height as u64))
            .map_err(|err| format!("Set input frame size failed: {err}"))?;
        input_type
            .SetUINT64(&MF_MT_FRAME_RATE, ((target_fps as u64) << 32) | 1)
            .map_err(|err| format!("Set input frame rate failed: {err}"))?;
        input_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)
            .map_err(|err| format!("Set input pixel aspect ratio failed: {err}"))?;
        input_type
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|err| format!("Set input interlace mode failed: {err}"))?;

        transform
            .SetInputType(0, &input_type, MFT_SET_TYPE_TEST_ONLY.0 as u32)
            .map_err(|err| format!("SetInputType(TEST_ONLY) failed: {err}"))?;
        transform
            .SetInputType(0, &input_type, 0)
            .map_err(|err| format!("SetInputType failed: {err}"))?;

        Ok(())
    }

    unsafe fn build_input_sample(
        nv12_bytes: &[u8],
        sample_time_hns: i64,
        sample_duration_hns: i64,
    ) -> Result<IMFSample, String> {
        let sample = MFCreateSample().map_err(|err| format!("MFCreateSample failed: {err}"))?;
        let buffer = MFCreateMemoryBuffer(nv12_bytes.len() as u32)
            .map_err(|err| format!("MFCreateMemoryBuffer failed: {err}"))?;

        let mut dst = null_mut();
        let mut max_len = 0u32;
        let mut cur_len = 0u32;
        buffer
            .Lock(&mut dst, Some(&mut max_len), Some(&mut cur_len))
            .map_err(|err| format!("IMFMediaBuffer::Lock failed: {err}"))?;
        if dst.is_null() {
            let _ = buffer.Unlock();
            return Err("IMFMediaBuffer::Lock returned null pointer".to_string());
        }

        std::ptr::copy_nonoverlapping(nv12_bytes.as_ptr(), dst, nv12_bytes.len());

        buffer
            .Unlock()
            .map_err(|err| format!("IMFMediaBuffer::Unlock failed: {err}"))?;
        buffer
            .SetCurrentLength(nv12_bytes.len() as u32)
            .map_err(|err| format!("IMFMediaBuffer::SetCurrentLength failed: {err}"))?;

        sample
            .AddBuffer(&buffer)
            .map_err(|err| format!("IMFSample::AddBuffer failed: {err}"))?;
        sample
            .SetSampleTime(sample_time_hns)
            .map_err(|err| format!("IMFSample::SetSampleTime failed: {err}"))?;
        sample
            .SetSampleDuration(sample_duration_hns)
            .map_err(|err| format!("IMFSample::SetSampleDuration failed: {err}"))?;

        Ok(sample)
    }

    unsafe fn drain_outputs_until_need_more_input(
        transform: &IMFTransform,
        output_stream_id: u32,
        output_sample_provided_by_mft: bool,
        output_buffer_size: u32,
    ) -> Result<Vec<EncodedAccessUnit>, String> {
        let mut collected = Vec::new();
        loop {
            match drain_one_output_sample(
                transform,
                output_stream_id,
                output_sample_provided_by_mft,
                output_buffer_size,
            )? {
                Some(unit) => collected.push(unit),
                None => break,
            }
        }
        Ok(collected)
    }

    unsafe fn drain_one_output_sample(
        transform: &IMFTransform,
        output_stream_id: u32,
        output_sample_provided_by_mft: bool,
        output_buffer_size: u32,
    ) -> Result<Option<EncodedAccessUnit>, String> {
        let mut output_sample = None;
        let mut output_buffer = None;

        if !output_sample_provided_by_mft {
            let sample = MFCreateSample().map_err(|err| format!("MFCreateSample(output) failed: {err}"))?;
            let buffer = MFCreateMemoryBuffer(output_buffer_size)
                .map_err(|err| format!("MFCreateMemoryBuffer(output) failed: {err}"))?;
            sample
                .AddBuffer(&buffer)
                .map_err(|err| format!("IMFSample::AddBuffer(output) failed: {err}"))?;
            output_sample = Some(sample);
            output_buffer = Some(buffer);
        }

        let mut output = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: output_stream_id,
            pSample: ManuallyDrop::new(output_sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        }];
        let mut status = 0u32;

        match transform.ProcessOutput(0, &mut output, &mut status) {
            Ok(()) => {}
            Err(err) if err.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                return Ok(None);
            }
            Err(err) => {
                return Err(format!("ProcessOutput failed: {err}"));
            }
        }

        let sample = output[0]
            .pSample
            .as_ref()
            .ok_or_else(|| "ProcessOutput returned no sample".to_string())?;
        let buffer: IMFMediaBuffer = if let Some(buf) = output_buffer {
            buf
        } else {
            let buf = sample
                .GetBufferByIndex(0)
                .map_err(|err| format!("GetBufferByIndex failed: {err}"))?;
            buf
        };

        let mut data_ptr = null_mut();
        let mut max_len = 0u32;
        let mut cur_len = 0u32;
        buffer
            .Lock(&mut data_ptr, Some(&mut max_len), Some(&mut cur_len))
            .map_err(|err| format!("Output buffer Lock failed: {err}"))?;
        if data_ptr.is_null() {
            let _ = buffer.Unlock();
            return Err("Output buffer Lock returned null pointer".to_string());
        }

        let bytes = std::slice::from_raw_parts(data_ptr, cur_len as usize).to_vec();
        buffer
            .Unlock()
            .map_err(|err| format!("Output buffer Unlock failed: {err}"))?;

        if bytes.is_empty() {
            return Ok(None);
        }

        Ok(Some(EncodedAccessUnit {
            data: bytes,
            keyframe: false,
        }))
    }
}

#[cfg(windows)]
pub use imp::*;

#[cfg(not(windows))]
mod imp_stub {
    pub struct EncodedAccessUnit {
        pub data: Vec<u8>,
        pub keyframe: bool,
    }

    pub struct MediaFoundationH264Encoder;
    pub struct MediaFoundationEncoderWorker;

    impl MediaFoundationH264Encoder {
        pub fn new(_width: usize, _height: usize, _target_fps: u32, _bitrate_bps: u32) -> Result<Self, String> {
            Err("Media Foundation H264 encoder is only available on Windows".to_string())
        }

        pub fn encode_nv12(
            &mut self,
            _width: usize,
            _height: usize,
            _nv12_bytes: &[u8],
        ) -> Result<Vec<EncodedAccessUnit>, String> {
            Err("Media Foundation H264 encoder is only available on Windows".to_string())
        }
    }

    impl MediaFoundationEncoderWorker {
        pub fn new(
            _initial_width: usize,
            _initial_height: usize,
            _target_fps: u32,
            _bitrate_bps: u32,
        ) -> Result<Self, String> {
            Err("Media Foundation H264 encoder is only available on Windows".to_string())
        }

        pub fn encode_nv12(
            &mut self,
            _width: usize,
            _height: usize,
            _nv12_bytes: Vec<u8>,
        ) -> Result<Vec<EncodedAccessUnit>, String> {
            Err("Media Foundation H264 encoder is only available on Windows".to_string())
        }
    }
}

#[cfg(not(windows))]
pub use imp_stub::*;
