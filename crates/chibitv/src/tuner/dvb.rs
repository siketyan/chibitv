use std::ffi::c_void;
use std::io::{ErrorKind, Read};
use std::ptr::{null, null_mut};

use anyhow::bail;
use dvbv5_sys::dvb_dev_type::{DVB_DEVICE_DEMUX, DVB_DEVICE_DVR, DVB_DEVICE_FRONTEND};
use dvbv5_sys::{
    DTV_FREQUENCY, DTV_STREAM_ID, dmx_output, dmx_ts_pes, dvb_dev_alloc, dvb_dev_close,
    dvb_dev_dmx_set_pesfilter, dvb_dev_find, dvb_dev_free, dvb_dev_list, dvb_dev_open,
    dvb_dev_read, dvb_dev_seek_by_adapter, dvb_dev_set_log, dvb_device, dvb_fe_set_parms,
    dvb_fe_store_parm, dvb_open_descriptor, dvb_set_compat_delivery_system, dvb_v5_fe_parms,
    fe_delivery_system,
};
use libc::{EOVERFLOW, O_RDONLY, O_RDWR};
use tracing::{error, info};

use crate::channel::ChannelInner;
use crate::tuner::{Channel, Tuner};

struct DvbDevice {
    dvb: *mut dvb_device,
    demux_dev: *mut dvb_dev_list,
    dvr_dev: *mut dvb_dev_list,
    fe_parms: *mut dvb_v5_fe_parms,
}

impl DvbDevice {
    fn open(adapter: u32, num: u32) -> anyhow::Result<Self> {
        unsafe {
            let dvb = dvb_dev_alloc();

            dvb_dev_set_log(dvb, 3, None);
            dvb_dev_find(dvb, None, null_mut());

            let demux_dev = dvb_dev_seek_by_adapter(dvb, adapter, num, DVB_DEVICE_DEMUX);
            if demux_dev.is_null() {
                bail!("Couldn't find demux device node");
            }

            let dvr_dev = dvb_dev_seek_by_adapter(dvb, adapter, num, DVB_DEVICE_DVR);
            if dvr_dev.is_null() {
                bail!("Couldn't find dvr device node");
            }

            let fe_dev = dvb_dev_seek_by_adapter(dvb, adapter, num, DVB_DEVICE_FRONTEND);
            if fe_dev.is_null() {
                bail!("Couldn't find frontend device node");
            }

            let fe_fd = dvb_dev_open(dvb, (*fe_dev).sysname, O_RDWR);
            if fe_fd.is_null() {
                bail!("Couldn't open the frontend device");
            }

            Ok(Self {
                dvb,
                demux_dev,
                dvr_dev,
                fe_parms: (*dvb).fe_parms,
            })
        }
    }

    fn open_demux(&self) -> anyhow::Result<DvbDemux> {
        unsafe {
            let fd = dvb_dev_open(self.dvb, (*self.demux_dev).sysname, O_RDWR);
            if fd.is_null() {
                bail!("Couldn't open the demux device");
            }

            Ok(DvbDemux { fd })
        }
    }

    fn open_dvr(&self) -> anyhow::Result<DvbDvr> {
        unsafe {
            let fd = dvb_dev_open(self.dvb, (*self.dvr_dev).sysname, O_RDONLY);
            if fd.is_null() {
                bail!("Couldn't open the dvr device");
            }

            Ok(DvbDvr { fd })
        }
    }
}

impl Drop for DvbDevice {
    fn drop(&mut self) {
        unsafe {
            dvb_dev_free(self.dvb);
        }
    }
}

unsafe impl Send for DvbDevice {}
unsafe impl Sync for DvbDevice {}

struct DvbDemux {
    fd: *mut dvb_open_descriptor,
}

impl Drop for DvbDemux {
    fn drop(&mut self) {
        unsafe {
            dvb_dev_close(self.fd);
        }
    }
}

unsafe impl Send for DvbDemux {}
unsafe impl Sync for DvbDemux {}

struct DvbDvr {
    fd: *mut dvb_open_descriptor,
}

impl Read for DvbDvr {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        unsafe {
            let ret = dvb_dev_read(self.fd, buf.as_mut_ptr() as *mut c_void, buf.len());
            if ret < 0 {
                if ret == (-EOVERFLOW as isize) {
                    error!("Buffer overrun!");
                }

                Err(ErrorKind::Other)?
            }

            Ok(ret as usize)
        }
    }
}

impl Drop for DvbDvr {
    fn drop(&mut self) {
        unsafe {
            dvb_dev_close(self.fd);
        }
    }
}

unsafe impl Send for DvbDvr {}
unsafe impl Sync for DvbDvr {}

pub struct DvbTuner {
    dev: DvbDevice,
    demux: DvbDemux,
}

impl DvbTuner {
    pub fn new(adapter_num: u8, frontend_num: u8) -> anyhow::Result<Self> {
        let dev = DvbDevice::open(adapter_num.into(), frontend_num.into())?;
        let demux_fd = dev.open_demux()?;

        Ok(Self {
            dev,
            demux: demux_fd,
        })
    }
}

impl Tuner for DvbTuner {
    fn open(&self) -> anyhow::Result<Box<dyn Read + Send + Sync>> {
        Ok(Box::new(self.dev.open_dvr()?))
    }

    fn tune(&self, channel: Channel) -> anyhow::Result<()> {
        unsafe {
            let p = self.dev.fe_parms;

            match channel.inner {
                ChannelInner::IsdbS {
                    frequency,
                    stream_id,
                } => {
                    info!("Tuning to {}, 0x{:#X}", frequency, stream_id);

                    // FIXME: It will be an invalid argument if this is omitted.
                    //        Maybe an issue around LNBf, but I don't know much about here :(
                    (*p).lnb = null();

                    dvb_set_compat_delivery_system(p, fe_delivery_system::SYS_ISDBS as u32);
                    dvb_fe_store_parm(p, DTV_FREQUENCY, frequency);
                    dvb_fe_store_parm(p, DTV_STREAM_ID, stream_id);
                }
            }

            dvb_fe_set_parms(p);

            dvb_dev_dmx_set_pesfilter(
                self.demux.fd,
                0x2000, // select all PIDs
                dmx_ts_pes::DMX_PES_OTHER,
                dmx_output::DMX_OUT_TS_TAP,
                0,
            );
        }

        Ok(())
    }
}
