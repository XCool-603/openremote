#[cfg(target_os = "windows")]
mod win_h264 {
    use anyhow::{anyhow, Result};
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed, ManuallyDrop};
    use std::ptr;
    use std::sync::Once;
    use windows::core::{Interface, GUID, HRESULT, VARIANT};
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
        D3D11_BIND_DECODER, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;
    use windows::Win32::Media::MediaFoundation::{
        eAVEncCommonRateControlMode_PeakConstrainedVBR, eAVEncH264VProfile_Main,
        CODECAPI_AVEncCommonMaxBitRate, CODECAPI_AVEncCommonMeanBitRate,
        CODECAPI_AVEncCommonQuality, CODECAPI_AVEncCommonQualityVsSpeed,
        CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
        CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode,
        ICodecAPI, IMFActivate,
        IMFDXGIBuffer, IMFDXGIDeviceManager, IMFMediaBuffer, IMFMediaEventGenerator,
        IMFMediaType, IMFSample, IMFTransform, METransformHaveOutput, METransformNeedInput,
        MFCreateDXGIDeviceManager, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
        MFStartup, MFSampleExtension_CleanPoint, MFTEnumEx, MFVideoArea,
        MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFSTARTUP_LITE,
        MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY,
        MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
        MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE,
        MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE, MF_SA_D3D11_BINDFLAGS, MF_TRANSFORM_ASYNC,
        MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION, MFT_CATEGORY_VIDEO_DECODER,
        MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE,
        MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_COMMAND_FLUSH,
        MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
        MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
        MFT_OUTPUT_STREAM_INFO, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO,
        MFMediaType_Video,
    };
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED,
    };

    static MF_INIT: Once = Once::new();

    pub fn ensure_mf_initialized() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        MF_INIT.call_once(|| unsafe {
            let _ = MFStartup(MF_VERSION, MFSTARTUP_LITE);
        });
    }

    #[inline]
    fn pack_u64(hi: u32, lo: u32) -> u64 {
        ((hi as u64) << 32) | (lo as u64)
    }

    #[inline]
    fn unpack_u64(v: u64) -> (u32, u32) {
        ((v >> 32) as u32, (v & 0xFFFF_FFFF) as u32)
    }

    fn set_codec_bool(mft: &IMFTransform, key: &GUID, val: bool) {
        if let Ok(codec_api) = mft.cast::<ICodecAPI>() {
            let v = VARIANT::from(val);
            unsafe {
                let _ = codec_api.SetValue(key, &v);
            }
        }
    }

    fn set_codec_u32(mft: &IMFTransform, key: &GUID, val: u32) {
        if let Ok(codec_api) = mft.cast::<ICodecAPI>() {
            let v = VARIANT::from(val);
            unsafe {
                let _ = codec_api.SetValue(key, &v);
            }
        }
    }

    /// Create a shared D3D11 device + IMFDXGIDeviceManager with video & multithread protection enabled.
    pub fn create_d3d11_video_device() -> Result<(ID3D11Device, ID3D11DeviceContext, IMFDXGIDeviceManager)> {
        ensure_mf_initialized();
        unsafe {
            let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;
            let flags = D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;

            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                flags,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .or_else(|_| {
                // Retry without VIDEO_SUPPORT flag on minimal adapters
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_HARDWARE,
                    None,
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&feature_levels),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            })
            .map_err(|e| anyhow!("D3D11CreateDevice failed: {}", e))?;

            let device = device.ok_or_else(|| anyhow!("Missing D3D11 device"))?;
            let context = context.ok_or_else(|| anyhow!("Missing D3D11 context"))?;

            if let Ok(mt) = device.cast::<ID3D11Multithread>() {
                let _ = mt.SetMultithreadProtected(true);
            }

            let mut reset_token = 0u32;
            let mut mgr: Option<IMFDXGIDeviceManager> = None;
            MFCreateDXGIDeviceManager(&mut reset_token, &mut mgr)
                .map_err(|e| anyhow!("MFCreateDXGIDeviceManager failed: {}", e))?;
            let mgr = mgr.ok_or_else(|| anyhow!("Missing DXGIDeviceManager"))?;
            mgr.ResetDevice(&device, reset_token)
                .map_err(|e| anyhow!("ResetDevice failed: {}", e))?;

            Ok((device, context, mgr))
        }
    }

    fn enum_mfts(
        category: GUID,
        flags: windows::Win32::Media::MediaFoundation::MFT_ENUM_FLAG,
        input_sub: Option<GUID>,
        output_sub: Option<GUID>,
    ) -> Result<Vec<IMFActivate>> {
        ensure_mf_initialized();
        unsafe {
            let in_info = input_sub.map(|sub| MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: sub,
            });
            let out_info = output_sub.map(|sub| MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: sub,
            });
            let mut ptr_arr: *mut Option<IMFActivate> = ptr::null_mut();
            let mut count = 0u32;
            MFTEnumEx(
                category,
                flags,
                in_info.as_ref().map(|r| r as *const _),
                out_info.as_ref().map(|r| r as *const _),
                &mut ptr_arr,
                &mut count,
            )
            .map_err(|e| anyhow!("MFTEnumEx failed: {}", e))?;

            let mut list = Vec::new();
            if !ptr_arr.is_null() && count > 0 {
                let slice = std::slice::from_raw_parts_mut(ptr_arr, count as usize);
                for slot in slice.iter_mut() {
                    if let Some(act) = slot.take() {
                        list.push(act);
                    }
                }
                CoTaskMemFree(Some(ptr_arr as *const c_void));
            }
            Ok(list)
        }
    }

    struct RawEncoder {
        mft: IMFTransform,
        event_gen: Option<IMFMediaEventGenerator>,
        is_hardware: bool,
        mft_provides_samples: bool,
        output_buf_min_size: u32,
        frame_index: u64,
        fps: u32,
    }

    impl RawEncoder {
        fn try_new(width: u32, height: u32, fps: u32, bitrate_bps: u32, prefer_hardware: bool) -> Result<Self> {
            let flags = if prefer_hardware {
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER
            } else {
                MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER
            };

            let activates = enum_mfts(
                MFT_CATEGORY_VIDEO_ENCODER,
                flags,
                Some(MFVideoFormat_NV12),
                Some(MFVideoFormat_H264),
            )?;
            if activates.is_empty() {
                return Err(anyhow!(
                    "No {} H.264 MFT encoder found",
                    if prefer_hardware { "hardware" } else { "software" }
                ));
            }

            let mut last_err = anyhow!("Failed initializing H.264 encoder");
            for act in activates {
                match Self::init_from_activate(&act, width, height, fps, bitrate_bps, prefer_hardware) {
                    Ok(enc) => return Ok(enc),
                    Err(e) => {
                        unsafe {
                            let _ = act.ShutdownObject();
                        }
                        last_err = e;
                    }
                }
            }
            Err(last_err)
        }

        fn init_from_activate(
            act: &IMFActivate,
            width: u32,
            height: u32,
            fps: u32,
            bitrate_bps: u32,
            is_hardware: bool,
        ) -> Result<Self> {
            unsafe {
                let mft: IMFTransform = act
                    .ActivateObject()
                    .map_err(|e| anyhow!("ActivateObject failed: {}", e))?;

                let mut is_async = false;
                if let Ok(attrs) = mft.GetAttributes() {
                    if let Ok(v) = attrs.GetUINT32(&MF_TRANSFORM_ASYNC) {
                        if v != 0 {
                            let _ = attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1);
                            is_async = true;
                        }
                    }
                    let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
                }

                set_codec_bool(&mft, &CODECAPI_AVLowLatencyMode, true);
                set_codec_u32(
                    &mft,
                    &CODECAPI_AVEncCommonRateControlMode,
                    eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32,
                );
                set_codec_u32(&mft, &CODECAPI_AVEncCommonQuality, 72);
                set_codec_u32(&mft, &CODECAPI_AVEncCommonMeanBitRate, bitrate_bps);
                set_codec_u32(
                    &mft,
                    &CODECAPI_AVEncCommonMaxBitRate,
                    bitrate_bps.saturating_mul(3) / 2,
                );
                set_codec_u32(&mft, &CODECAPI_AVEncMPVDefaultBPictureCount, 0);
                set_codec_u32(&mft, &CODECAPI_AVEncMPVGOPSize, fps.saturating_mul(8).max(120));
                set_codec_u32(&mft, &CODECAPI_AVEncCommonQualityVsSpeed, 0);

                // Configure output media type (H.264 Annex-B)
                let out_type: IMFMediaType = MFCreateMediaType()
                    .map_err(|e| anyhow!("MFCreateMediaType(out) failed: {}", e))?;
                out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
                out_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_bps)?;
                out_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
                out_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
                out_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                out_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)?;

                mft.SetOutputType(0, &out_type, 0)
                    .map_err(|e| anyhow!("Encoder SetOutputType failed: {}", e))?;

                // Configure input media type (NV12)
                let in_type: IMFMediaType = MFCreateMediaType()
                    .map_err(|e| anyhow!("MFCreateMediaType(in) failed: {}", e))?;
                in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
                in_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
                in_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
                in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                in_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;

                mft.SetInputType(0, &in_type, 0)
                    .map_err(|e| anyhow!("Encoder SetInputType failed: {}", e))?;

                let out_info: MFT_OUTPUT_STREAM_INFO = mft
                    .GetOutputStreamInfo(0)
                    .map_err(|e| anyhow!("GetOutputStreamInfo failed: {}", e))?;
                let mft_provides_samples =
                    (out_info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32)) != 0;

                mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
                mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

                let event_gen = if is_async {
                    mft.cast::<IMFMediaEventGenerator>().ok()
                } else {
                    None
                };

                Ok(Self {
                    mft,
                    event_gen,
                    is_hardware,
                    mft_provides_samples,
                    output_buf_min_size: out_info.cbSize.max(256 * 1024),
                    frame_index: 0,
                    fps,
                })
            }
        }

        fn read_output_once(&self) -> Result<Option<(Vec<u8>, bool)>> {
            unsafe {
                let allocated_sample: Option<IMFSample> = if self.mft_provides_samples {
                    None
                } else {
                    let s: IMFSample = MFCreateSample()?;
                    let b: IMFMediaBuffer = MFCreateMemoryBuffer(self.output_buf_min_size)?;
                    s.AddBuffer(&b)?;
                    Some(s)
                };

                let mut out_buf = MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(allocated_sample),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                };

                let mut status = 0u32;
                let res = self.mft.ProcessOutput(0, std::slice::from_mut(&mut out_buf), &mut status);

                let out_sample = ManuallyDrop::take(&mut out_buf.pSample);
                let _ = ManuallyDrop::take(&mut out_buf.pEvents);

                match res {
                    Ok(()) => {
                        let sample = out_sample.ok_or_else(|| anyhow!("Encoder returned empty sample"))?;
                        let buf = sample.ConvertToContiguousBuffer()?;
                        let mut ptr_data: *mut u8 = ptr::null_mut();
                        let mut len = 0u32;
                        buf.Lock(&mut ptr_data, None, Some(&mut len))?;
                        let bytes = if !ptr_data.is_null() && len > 0 {
                            std::slice::from_raw_parts(ptr_data, len as usize).to_vec()
                        } else {
                            Vec::new()
                        };
                        let _ = buf.Unlock();
                        let clean = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
                        Ok(Some((bytes, clean)))
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(None),
                    Err(e) => Err(anyhow!("Encoder ProcessOutput failed: {}", e)),
                }
            }
        }

        fn encode_nv12(&mut self, nv12: &[u8], force_keyframe: bool) -> Result<(Vec<u8>, bool)> {
            unsafe {
                if force_keyframe {
                    set_codec_u32(&self.mft, &CODECAPI_AVEncVideoForceKeyFrame, 1);
                }

                let sample: IMFSample = MFCreateSample()?;
                let buf: IMFMediaBuffer = MFCreateMemoryBuffer(nv12.len() as u32)?;
                {
                    let mut dst_ptr: *mut u8 = ptr::null_mut();
                    buf.Lock(&mut dst_ptr, None, None)?;
                    ptr::copy_nonoverlapping(nv12.as_ptr(), dst_ptr, nv12.len());
                    buf.Unlock()?;
                }
                buf.SetCurrentLength(nv12.len() as u32)?;
                sample.AddBuffer(&buf)?;

                let dur = (10_000_000i64) / (self.fps.max(1) as i64);
                let pts = (self.frame_index as i64) * dur;
                self.frame_index += 1;
                sample.SetSampleTime(pts)?;
                sample.SetSampleDuration(dur)?;

                let mut result_bytes = Vec::new();
                let mut is_key = false;

                if let Some(ref gen) = self.event_gen {
                    let mut input_fed = false;
                    for _ in 0..32 {
                        let ev = gen.GetEvent(windows::Win32::Media::MediaFoundation::MF_EVENT_FLAG_NONE)?;
                        let ev_type = ev.GetType()?;
                        if ev_type == METransformNeedInput.0 as u32 && !input_fed {
                            self.mft.ProcessInput(0, &sample, 0)?;
                            input_fed = true;
                        } else if ev_type == METransformHaveOutput.0 as u32 {
                            if let Some((chunk, clean)) = self.read_output_once()? {
                                result_bytes.extend_from_slice(&chunk);
                                is_key |= clean;
                                if input_fed {
                                    break;
                                }
                            }
                        }
                    }
                } else {
                    self.mft.ProcessInput(0, &sample, 0)?;
                    while let Some((chunk, clean)) = self.read_output_once()? {
                        result_bytes.extend_from_slice(&chunk);
                        is_key |= clean;
                    }
                }

                Ok((result_bytes, is_key))
            }
        }
    }

    impl Drop for RawEncoder {
        fn drop(&mut self) {
            unsafe {
                let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
                let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            }
        }
    }

    /// Automatic Hardware-First H.264 Encoder (NVENC / QSV / AMF -> Software MFT fallback),
    /// dynamically rebuilding whenever resolution or bitrate tier changes.
    pub struct H264Encoder {
        inner: Option<RawEncoder>,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_bps: u32,
        hardware_disabled: bool,
    }

    impl H264Encoder {
        pub fn new(width: u32, height: u32, fps: u32, bitrate_bps: u32) -> Result<Self> {
            let mut enc = Self {
                inner: None,
                width: 0,
                height: 0,
                fps: fps.clamp(10, 60),
                bitrate_bps: bitrate_bps.max(400_000),
                hardware_disabled: false,
            };
            enc.ensure_config(width, height, fps, bitrate_bps)?;
            Ok(enc)
        }

        pub fn backend_name(&self) -> &'static str {
            match self.inner {
                Some(ref r) if r.is_hardware => "Hardware-MFT",
                Some(_) => "Software-MFT",
                None => "Uninitialized",
            }
        }

        pub fn ensure_config(&mut self, width: u32, height: u32, fps: u32, bitrate_bps: u32) -> Result<()> {
            let w = (width & !1).max(16);
            let h = (height & !1).max(16);
            let fps = fps.clamp(10, 60);
            let bitrate_bps = bitrate_bps.max(400_000);

            let bitrate_changed = (self.bitrate_bps as i64 - bitrate_bps as i64).abs() > 350_000;
            if self.inner.is_some()
                && self.width == w
                && self.height == h
                && self.fps == fps
                && !bitrate_changed
            {
                return Ok(());
            }

            if self.inner.is_some() && self.width == w && self.height == h && self.fps == fps && bitrate_changed {
                if let Some(ref raw) = self.inner {
                    set_codec_u32(&raw.mft, &CODECAPI_AVEncCommonMeanBitRate, bitrate_bps);
                    set_codec_u32(
                        &raw.mft,
                        &CODECAPI_AVEncCommonMaxBitRate,
                        bitrate_bps.saturating_mul(3) / 2,
                    );
                    self.bitrate_bps = bitrate_bps;
                    return Ok(());
                }
            }

            self.inner = None;
            let raw = if !self.hardware_disabled {
                match RawEncoder::try_new(w, h, fps, bitrate_bps, true) {
                    Ok(r) => r,
                    Err(e) => {
                        log::warn!("Hardware H.264 MFT unavailable ({}), falling back to Software H.264 MFT", e);
                        self.hardware_disabled = true;
                        RawEncoder::try_new(w, h, fps, bitrate_bps, false)?
                    }
                }
            } else {
                RawEncoder::try_new(w, h, fps, bitrate_bps, false)?
            };

            self.width = w;
            self.height = h;
            self.fps = fps;
            self.bitrate_bps = bitrate_bps;
            self.inner = Some(raw);
            Ok(())
        }

        pub fn encode_nv12(
            &mut self,
            width: u32,
            height: u32,
            fps: u32,
            bitrate_bps: u32,
            nv12: &[u8],
            force_keyframe: bool,
        ) -> Result<(Vec<u8>, bool)> {
            self.ensure_config(width, height, fps, bitrate_bps)?;
            let first_attempt = self
                .inner
                .as_mut()
                .ok_or_else(|| anyhow!("Missing H264 encoder"))?
                .encode_nv12(nv12, force_keyframe);

            match first_attempt {
                Ok(out) => Ok(out),
                Err(e) if !self.hardware_disabled => {
                    log::warn!("Hardware MFT encode failed ({}), switching seamlessly to Software MFT", e);
                    self.hardware_disabled = true;
                    self.inner = None;
                    self.ensure_config(self.width, self.height, self.fps, self.bitrate_bps)?;
                    self.inner
                        .as_mut()
                        .ok_or_else(|| anyhow!("Missing H264 fallback encoder"))?
                        .encode_nv12(nv12, true)
                }
                Err(e) => Err(e),
            }
        }
    }

    unsafe impl Send for H264Encoder {}

    /// Output of hardware D3D11 (zero-copy GPU texture) or CPU NV12 fallback H.264 decode
    pub enum DecodedNv12Frame {
        GpuTexture {
            texture: ID3D11Texture2D,
            subresource: u32,
            tex_w: u32,
            tex_h: u32,
            visible_w: u32,
            visible_h: u32,
        },
        CpuBuffer {
            data: Vec<u8>,
            tex_w: u32,
            tex_h: u32,
            visible_w: u32,
            visible_h: u32,
        },
    }

    /// Hardware DXVA / D3D11-backed Windows Media Foundation H.264 Decoder
    pub struct H264Decoder {
        mft: IMFTransform,
        d3d_device: ID3D11Device,
        _d3d_mgr: IMFDXGIDeviceManager,
        mft_provides_samples: bool,
        output_buf_min_size: u32,
        tex_w: u32,
        tex_h: u32,
        visible_w: u32,
        visible_h: u32,
        pts_counter: u64,
    }

    impl H264Decoder {
        pub fn new(d3d_device: ID3D11Device, d3d_mgr: IMFDXGIDeviceManager) -> Result<Self> {
            ensure_mf_initialized();
            unsafe {
                let activates = enum_mfts(
                    MFT_CATEGORY_VIDEO_DECODER,
                    MFT_ENUM_FLAG_SYNCMFT
                        | MFT_ENUM_FLAG_ASYNCMFT
                        | MFT_ENUM_FLAG_HARDWARE
                        | MFT_ENUM_FLAG_SORTANDFILTER,
                    Some(MFVideoFormat_H264),
                    Some(MFVideoFormat_NV12),
                )?;
                if activates.is_empty() {
                    return Err(anyhow!("No MediaFoundation H.264 decoder found"));
                }

                let mut last_err = anyhow!("Failed activating H.264 decoder");
                for act in activates {
                    match Self::try_init_decoder(&act, d3d_device.clone(), d3d_mgr.clone()) {
                        Ok(dec) => return Ok(dec),
                        Err(e) => {
                            let _ = act.ShutdownObject();
                            last_err = e;
                        }
                    }
                }
                Err(last_err)
            }
        }

        fn try_init_decoder(
            act: &IMFActivate,
            d3d_device: ID3D11Device,
            d3d_mgr: IMFDXGIDeviceManager,
        ) -> Result<Self> {
            unsafe {
                let mft: IMFTransform = act.ActivateObject()?;
                if let Ok(attrs) = mft.GetAttributes() {
                    if let Ok(v) = attrs.GetUINT32(&MF_TRANSFORM_ASYNC) {
                        if v != 0 {
                            let _ = attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1);
                        }
                    }
                    let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
                }

                // Bind D3D11 Device Manager for DXVA/D3D11 GPU zero-copy decode
                let mgr_raw = d3d_mgr.as_raw() as usize;
                let _ = mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, mgr_raw);

                if let Ok(out_attrs) = mft.GetOutputStreamAttributes(0) {
                    let bind_flags = (D3D11_BIND_DECODER.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
                    let _ = out_attrs.SetUINT32(&MF_SA_D3D11_BINDFLAGS, bind_flags);
                }
                set_codec_bool(&mft, &CODECAPI_AVLowLatencyMode, true);

                let in_type: IMFMediaType = MFCreateMediaType()?;
                in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
                in_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(1920, 1080))?;
                mft.SetInputType(0, &in_type, 0)?;

                let mut dec = Self {
                    mft,
                    d3d_device,
                    _d3d_mgr: d3d_mgr,
                    mft_provides_samples: false,
                    output_buf_min_size: 1920 * 1088 * 2,
                    tex_w: 1920,
                    tex_h: 1080,
                    visible_w: 1920,
                    visible_h: 1080,
                    pts_counter: 0,
                };
                dec.negotiate_output_type()?;
                dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
                dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
                Ok(dec)
            }
        }

        fn negotiate_output_type(&mut self) -> Result<()> {
            unsafe {
                let mut selected: Option<IMFMediaType> = None;
                let mut idx = 0u32;
                loop {
                    match self.mft.GetOutputAvailableType(0, idx) {
                        Ok(mt) => {
                            if let Ok(sub) = mt.GetGUID(&MF_MT_SUBTYPE) {
                                if sub == MFVideoFormat_NV12 {
                                    selected = Some(mt);
                                    break;
                                }
                            }
                            idx += 1;
                        }
                        Err(_) => break,
                    }
                }
                let out_type = match selected {
                    Some(mt) => mt,
                    None => {
                        let mt = MFCreateMediaType()?;
                        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                        mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
                        mt.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(1920, 1080))?;
                        mt
                    }
                };
                self.mft.SetOutputType(0, &out_type, 0)?;

                if let Ok(cur_mt) = self.mft.GetOutputCurrentType(0) {
                    self.read_dimensions_from_type(&cur_mt);
                }

                let out_info = self.mft.GetOutputStreamInfo(0)?;
                self.mft_provides_samples =
                    (out_info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32)) != 0;
                self.output_buf_min_size = out_info
                    .cbSize
                    .max((self.tex_w * self.tex_h * 3) / 2)
                    .max(64 * 1024);
                Ok(())
            }
        }

        fn read_dimensions_from_type(&mut self, mt: &IMFMediaType) {
            unsafe {
                if let Ok(packed) = mt.GetUINT64(&MF_MT_FRAME_SIZE) {
                    let (w, h) = unpack_u64(packed);
                    if w > 0 && h > 0 {
                        self.tex_w = w;
                        self.tex_h = h;
                        self.visible_w = w;
                        self.visible_h = h;
                    }
                }
                // Respect active crop aperture (prevents bottom 8px green bar on 1080p -> 1088p H.264!)
                let mut area: MFVideoArea = zeroed();
                let mut got_size = 0u32;
                if mt
                    .GetBlob(
                        &MF_MT_MINIMUM_DISPLAY_APERTURE,
                        std::slice::from_raw_parts_mut(
                            &mut area as *mut MFVideoArea as *mut u8,
                            size_of::<MFVideoArea>(),
                        ),
                        Some(&mut got_size),
                    )
                    .is_ok()
                    && got_size as usize == size_of::<MFVideoArea>()
                {
                    if area.Area.cx > 0 && area.Area.cy > 0 {
                        self.visible_w = area.Area.cx as u32;
                        self.visible_h = area.Area.cy as u32;
                    }
                }
            }
        }

        pub fn decode(&mut self, annex_b: &[u8], hinted_w: u16, hinted_h: u16) -> Result<Option<DecodedNv12Frame>> {
            if annex_b.is_empty() {
                return Ok(None);
            }
            unsafe {
                let sample: IMFSample = MFCreateSample()?;
                let buf: IMFMediaBuffer = MFCreateMemoryBuffer(annex_b.len() as u32)?;
                {
                    let mut dst_ptr: *mut u8 = ptr::null_mut();
                    buf.Lock(&mut dst_ptr, None, None)?;
                    ptr::copy_nonoverlapping(annex_b.as_ptr(), dst_ptr, annex_b.len());
                    buf.Unlock()?;
                }
                buf.SetCurrentLength(annex_b.len() as u32)?;
                sample.AddBuffer(&buf)?;
                let pts = (self.pts_counter as i64) * 166_667;
                self.pts_counter += 1;
                sample.SetSampleTime(pts)?;
                sample.SetSampleDuration(166_667)?;

                self.mft.ProcessInput(0, &sample, 0)?;

                let mut latest: Option<DecodedNv12Frame> = None;
                for _ in 0..8 {
                    let alloc_sample: Option<IMFSample> = if self.mft_provides_samples {
                        None
                    } else {
                        let s = MFCreateSample()?;
                        let b = MFCreateMemoryBuffer(self.output_buf_min_size)?;
                        s.AddBuffer(&b)?;
                        Some(s)
                    };

                    let mut out_buf = MFT_OUTPUT_DATA_BUFFER {
                        dwStreamID: 0,
                        pSample: ManuallyDrop::new(alloc_sample),
                        dwStatus: 0,
                        pEvents: ManuallyDrop::new(None),
                    };
                    let mut status = 0u32;
                    let res = self.mft.ProcessOutput(0, std::slice::from_mut(&mut out_buf), &mut status);
                    let out_sample = ManuallyDrop::take(&mut out_buf.pSample);
                    let _ = ManuallyDrop::take(&mut out_buf.pEvents);

                    match res {
                        Ok(()) => {
                            if let Some(s) = out_sample {
                                let buf = s.GetBufferByIndex(0)?;
                                let vis_w = if hinted_w > 0 && (hinted_w as u32) <= self.tex_w {
                                    hinted_w as u32
                                } else {
                                    self.visible_w
                                };
                                let vis_h = if hinted_h > 0 && (hinted_h as u32) <= self.tex_h {
                                    hinted_h as u32
                                } else {
                                    self.visible_h
                                };

                                if let Ok(dxgi_buf) = buf.cast::<IMFDXGIBuffer>() {
                                    let mut tex_ptr: *mut c_void = ptr::null_mut();
                                    dxgi_buf.GetResource(&ID3D11Texture2D::IID, &mut tex_ptr)?;
                                    if tex_ptr.is_null() {
                                        return Err(anyhow!("IMFDXGIBuffer::GetResource returned null"));
                                    }
                                    let tex = ID3D11Texture2D::from_raw(tex_ptr);
                                    let subresource = dxgi_buf.GetSubresourceIndex()?;
                                    let mut desc: D3D11_TEXTURE2D_DESC = zeroed();
                                    tex.GetDesc(&mut desc);
                                    let tw = desc.Width.max(self.tex_w);
                                    let th = desc.Height.max(self.tex_h);
                                    latest = Some(DecodedNv12Frame::GpuTexture {
                                        texture: tex,
                                        subresource,
                                        tex_w: tw,
                                        tex_h: th,
                                        visible_w: vis_w.min(tw),
                                        visible_h: vis_h.min(th),
                                    });
                                } else {
                                    let mut p: *mut u8 = ptr::null_mut();
                                    let mut len = 0u32;
                                    buf.Lock(&mut p, None, Some(&mut len))?;
                                    let data = if !p.is_null() && len > 0 {
                                        std::slice::from_raw_parts(p, len as usize).to_vec()
                                    } else {
                                        Vec::new()
                                    };
                                    let _ = buf.Unlock();
                                    if !data.is_empty() {
                                        latest = Some(DecodedNv12Frame::CpuBuffer {
                                            data,
                                            tex_w: self.tex_w,
                                            tex_h: self.tex_h,
                                            visible_w: vis_w.min(self.tex_w),
                                            visible_h: vis_h.min(self.tex_h),
                                        });
                                    }
                                }
                            }
                        }
                        Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                            self.negotiate_output_type()?;
                        }
                        Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                            break;
                        }
                        Err(e) => {
                            return Err(anyhow!("H264Decoder ProcessOutput failed: {} ({:?})", e, e.code()));
                        }
                    }
                }

                // Keep field alive check
                let _ = &self.d3d_device;
                let _ = DXGI_FORMAT_NV12;
                let _ = HRESULT(0);
                Ok(latest)
            }
        }
    }

    impl Drop for H264Decoder {
        fn drop(&mut self) {
            unsafe {
                let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
                let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            }
        }
    }

    unsafe impl Send for H264Decoder {}
}

#[cfg(target_os = "windows")]
pub use win_h264::{create_d3d11_video_device, DecodedNv12Frame, H264Decoder, H264Encoder};

#[cfg(not(target_os = "windows"))]
mod stub_h264 {
    use anyhow::{anyhow, Result};

    pub struct H264Encoder;
    impl H264Encoder {
        pub fn new(_w: u32, _h: u32, _fps: u32, _bitrate: u32) -> Result<Self> {
            Err(anyhow!("H.264 MFT only supported on Windows"))
        }
        pub fn backend_name(&self) -> &'static str {
            "Unsupported"
        }
        pub fn encode_nv12(
            &mut self,
            _w: u32,
            _h: u32,
            _fps: u32,
            _bitrate: u32,
            _nv12: &[u8],
            _kf: bool,
        ) -> Result<(Vec<u8>, bool)> {
            Err(anyhow!("Unsupported"))
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub use stub_h264::H264Encoder;

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn test_h264_encode_decode_gpu_roundtrip() {
        let (w, h) = (640u32, 360u32);
        let mut enc = H264Encoder::new(w, h, 60, 2_000_000).expect("H264Encoder init");
        println!("H.264 encoder active backend: {}", enc.backend_name());

        let (dev, _ctx, mgr) = create_d3d11_video_device().expect("D3D11 video device");
        let mut dec = H264Decoder::new(dev, mgr).expect("H264Decoder init");

        // Build a synthetic NV12 gradient frame
        let y_len = (w * h) as usize;
        let mut nv12 = vec![128u8; y_len + y_len / 2];
        for i in 0..y_len {
            nv12[i] = ((i % w as usize) * 200 / w as usize + 16) as u8;
        }

        let mut decoded_count = 0usize;
        for frame_idx in 0..4 {
            nv12[0] = (20 + frame_idx * 15) as u8;
            let (annex_b, is_key) = enc
                .encode_nv12(w, h, 60, 2_000_000, &nv12, frame_idx == 0)
                .expect("encode_nv12");
            assert!(!annex_b.is_empty(), "Encoded H.264 payload must not be empty");
            println!(
                "Encoded frame #{}: {} bytes, keyframe={}",
                frame_idx,
                annex_b.len(),
                is_key
            );
            if let Some(decoded) = dec.decode(&annex_b, w as u16, h as u16).expect("decode") {
                decoded_count += 1;
                match decoded {
                    DecodedNv12Frame::GpuTexture { visible_w, visible_h, tex_w, tex_h, .. } => {
                        println!(
                            "Decoded GPU texture frame: visible={}x{}, tex={}x{}",
                            visible_w, visible_h, tex_w, tex_h
                        );
                        assert_eq!((visible_w, visible_h), (w, h));
                    }
                    DecodedNv12Frame::CpuBuffer { visible_w, visible_h, tex_w, tex_h, .. } => {
                        println!(
                            "Decoded CPU NV12 frame: visible={}x{}, tex={}x{}",
                            visible_w, visible_h, tex_w, tex_h
                        );
                        assert_eq!((visible_w, visible_h), (w, h));
                    }
                }
            }
        }
        assert!(decoded_count >= 1, "H264Decoder should produce at least 1 decoded frame");
    }

    #[test]
    fn bench_h264_720p_1080p_timing() {
        for (w, h) in [(1280u32, 800u32), (1920u32, 1080u32)] {
            let mut enc = H264Encoder::new(w, h, 30, 2_000_000).expect("enc");
            let (dev, _ctx, mgr) = create_d3d11_video_device().expect("d3d11");
            let mut dec = H264Decoder::new(dev, mgr).expect("dec");
            let y_len = (w * h) as usize;
            let mut nv12 = vec![128u8; y_len + y_len / 2];
            for i in 0..y_len {
                nv12[i] = ((i % w as usize) * 200 / w as usize + 16) as u8;
            }
            let t0 = std::time::Instant::now();
            let mut total_bytes = 0usize;
            for f in 0..10 {
                nv12[f * 17] = ((f * 31) % 240) as u8;
                let (pkt, _) = enc.encode_nv12(w, h, 30, 2_000_000, &nv12, f == 0).expect("enc");
                total_bytes += pkt.len();
                let _ = dec.decode(&pkt, w as u16, h as u16).expect("dec");
            }
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            println!(
                "{}x{} [{}] 10 frames enc+dec: {:.1} ms total ({:.1} ms/frame), total_bytes={} ({:.1} KB/frame)",
                w,
                h,
                enc.backend_name(),
                ms,
                ms / 10.0,
                total_bytes,
                total_bytes as f64 / 10240.0
            );
        }
    }
}
