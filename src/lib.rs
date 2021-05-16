//! Rust interface for writing Slurm SPANK Plugins

use byte_strings::{c_str, concat_bytes};
use lazy_static::lazy_static;
use libc::{gid_t, pid_t, uid_t};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use std::borrow::Cow;
use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::error::Error;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fmt;
use std::os::raw::{c_char, c_int};
use std::os::unix::ffi::OsStrExt;
use std::panic::catch_unwind;
use std::sync::Mutex;
use std::{ptr, slice};

mod bindings;

#[repr(C)]
#[doc(hidden)]
pub struct StaticCStr(*const u8);
unsafe impl Sync for StaticCStr {}

#[derive(Default)]
struct OptionCache {
    options: Vec<String>,
    values: HashMap<String, Vec<Option<OsString>>>,
}

/// This struct represents a handle to the Slurm interface exposed to SPANK plugins. It provides methods
/// to query Slurm from your plugin.
pub struct SpankHandle<'a> {
    spank: bindings::spank_t,
    opt_cache: &'a mut OptionCache,
    argc: c_int,
    argv: *const *const c_char,
}

macro_rules! spank_item_getter {
    ($name:ident, $spank_item:path, $arg_name:ident, $arg_type:ty, $result_type:ty) => {
        pub fn $name(&self, $arg_name: $arg_type) -> Result<$result_type, SpankError> {
            let mut res: $result_type = <$result_type>::default();
            let res_ptr: *mut $result_type = &mut res;
            match unsafe {
                bindings::spank_get_item(self.spank, $spank_item.into(), $arg_name, res_ptr)
            } {
                bindings::spank_err_ESPANK_SUCCESS => Ok(res),
                e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
            }
        }
    };
    ($name:ident, $spank_item:path, &str) => {
        pub fn $name(&self) -> Result<&str, SpankError> {
            let mut res: *const c_char = ptr::null_mut();
            let res_ptr: *mut *const c_char = &mut res;
            match unsafe { bindings::spank_get_item(self.spank, $spank_item.into(), res_ptr) } {
                bindings::spank_err_ESPANK_SUCCESS => {
                    if res.is_null() {
                        Err(SpankError::Generic)
                    } else {
                        unsafe { CStr::from_ptr(res) }
                            .to_str()
                            .map_err(|_| SpankError::Generic)
                    }
                }
                e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
            }
        }
    };
    ($name:ident, $spank_item:path,$result_type:ty) => {
        pub fn $name(&self) -> Result<$result_type, SpankError> {
            let mut res: $result_type = <$result_type>::default();
            let res_ptr: *mut $result_type = &mut res;
            match unsafe { bindings::spank_get_item(self.spank, $spank_item.into(), res_ptr) } {
                bindings::spank_err_ESPANK_SUCCESS => Ok(res),
                e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
            }
        }
    };
}

impl<'a> SpankHandle<'a> {
    /// Returns the context in which the calling plugin is loaded.
    pub fn context(&self) -> Result<Context, SpankError> {
        let ctx = unsafe { bindings::spank_context() };
        Context::try_from(ctx).map_err(|_| SpankError::Generic)
    }

    /// Registers a plugin-provided option dynamically. This function
    /// is only valid when called from your plugin's `init()`, and must
    /// be guaranteed to be called in all contexts in which it is
    /// used (local, remote, allocator).
    pub fn register_option(&mut self, spank_opt: SpankOption) -> Result<(), SpankError> {
        let arginfo = match spank_opt.arginfo {
            None => None,
            Some(info) => Some(CString::new(&info as &str).or(Err(SpankError::BadArg))?),
        };
        let name = CString::new(&spank_opt.name as &str).or(Err(SpankError::BadArg))?;
        let usage = match spank_opt.usage {
            None => None,
            Some(usage) => Some(CString::new(&usage as &str).or(Err(SpankError::BadArg))?),
        };

        let mut c_spank_opt = bindings::spank_option {
            name: name.as_ptr(),
            has_arg: arginfo.is_some() as i32,
            cb: Some(spank_opt_callback),
            arginfo: match arginfo {
                Some(ref arginfo) => arginfo.as_ptr(),
                None => ptr::null(),
            },
            usage: match usage {
                Some(ref usage) => usage.as_ptr(),
                None => ptr::null(),
            },
            val: self
                .opt_cache
                .options
                .len()
                .try_into()
                .or(Err(SpankError::NoSpace))?,
        };

        match unsafe { bindings::spank_option_register(self.spank, &mut c_spank_opt) } {
            bindings::spank_err_ESPANK_SUCCESS => {
                self.opt_cache.options.push(spank_opt.name);
                Ok(())
            }
            e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
        }
    }

    /// Returns the list of arguments configured in the `plugstack.conf` file for this plugin
    pub fn plugin_argv(&self) -> Result<Vec<&str>, SpankError> {
        self.argv_to_vec(self.argc as usize, self.argv)
    }

    // TODO: Return a better error type
    fn argv_to_vec(
        &self,
        argc: usize,
        argv: *const *const c_char,
    ) -> Result<Vec<&str>, SpankError> {
        unsafe { slice::from_raw_parts(argv, argc) }
            .iter()
            .map(|&arg| unsafe { CStr::from_ptr(arg) }.to_str())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SpankError::Generic)
    }

    fn argv_to_vec_os(&self, argc: usize, argv: *const *const c_char) -> Vec<&OsStr> {
        unsafe { slice::from_raw_parts(argv, argc) }
            .iter()
            .map(|&arg| OsStr::from_bytes(unsafe { CStr::from_ptr(arg) }.to_bytes()))
            .collect()
    }

    fn do_getenv_os<N: AsRef<OsStr>>(
        &self,
        name: N,
        spank_fn: unsafe extern "C" fn(
            bindings::spank_t,
            *const c_char,
            *mut c_char,
            c_int,
        ) -> bindings::spank_err_t,
    ) -> Result<&OsStr, SpankError> {
        let mut max_size = 4096;
        let c_name = CString::new(name.as_ref().as_bytes()).map_err(|_| SpankError::BadArg)?;
        let mut buffer = Vec::<c_char>::with_capacity(max_size);
        loop {
            buffer.resize(max_size, 0);
            let buffer_ptr = buffer.as_mut_ptr();

            match unsafe { spank_fn(self.spank, c_name.as_ptr(), buffer_ptr, max_size as i32) } {
                bindings::spank_err_ESPANK_NOSPACE => {
                    max_size *= 2;
                    continue;
                }
                bindings::spank_err_ESPANK_SUCCESS => {
                    return Ok(OsStr::from_bytes(
                        unsafe { CStr::from_ptr(buffer_ptr) }.to_bytes(),
                    ))
                }
                e => return Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
            }
        }
    }

    ///  Retrieves the environment variable `name` from the job's environment
    pub fn getenv<N: AsRef<OsStr>>(&self, name: N) -> Result<&str, SpankError> {
        self.do_getenv_os(name, bindings::spank_getenv)
            .and_then(|env| env.to_str().ok_or(SpankError::Generic))
    }

    ///  Retrieves the environment variable `name` from the job's environment
    pub fn getenv_os_lossy<N: AsRef<OsStr>>(&self, name: N) -> Result<Cow<'_, str>, SpankError> {
        self.do_getenv_os(name, bindings::spank_getenv)
            .map(|s| s.to_string_lossy())
    }

    ///  Retrieves the environment variable `name` from the job's environment
    pub fn getenv_os<N: AsRef<OsStr>>(&self, name: N) -> Result<&OsStr, SpankError> {
        self.do_getenv_os(name, bindings::spank_getenv)
    }

    ///  Retrieves the environment variable `name` from the job's control environment
    pub fn job_control_getenv<N: AsRef<OsStr>>(&self, name: N) -> Result<&str, SpankError> {
        self.do_getenv_os(name, bindings::spank_job_control_getenv)
            .and_then(|env| env.to_str().ok_or(SpankError::Generic))
    }

    ///  Retrieves the environment variable `name` from the job's control environment
    pub fn job_control_getenv_lossy<N: AsRef<OsStr>>(
        &self,
        name: N,
    ) -> Result<Cow<'_, str>, SpankError> {
        self.do_getenv_os(name, bindings::spank_job_control_getenv)
            .map(|s| s.to_string_lossy())
    }

    ///  Retrieves the environment variable `name` from the job's control environment
    pub fn job_control_getenv_os<N: AsRef<OsStr>>(&self, name: N) -> Result<&OsStr, SpankError> {
        self.do_getenv_os(name, bindings::spank_job_control_getenv)
    }

    pub fn do_setenv<N: AsRef<OsStr>, V: AsRef<OsStr>>(
        &self,
        name: N,
        value: V,
        overwrite: bool,
        spank_fn: unsafe extern "C" fn(
            bindings::spank_t,
            *const c_char,
            *const c_char,
            c_int,
        ) -> bindings::spank_err_t,
    ) -> Result<(), SpankError> {
        let c_name = CString::new(name.as_ref().as_bytes()).map_err(|_| SpankError::BadArg)?;
        let c_value = CString::new(value.as_ref().as_bytes()).map_err(|_| SpankError::BadArg)?;

        match unsafe {
            spank_fn(
                self.spank,
                c_name.as_ptr(),
                c_value.as_ptr(),
                overwrite as c_int,
            )
        } {
            bindings::spank_err_ESPANK_SUCCESS => Ok(()),
            e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
        }
    }

    /// Sets the environment variable `name` in the job's control environment to the provided `value`.
    ///
    /// Existing values will be overwritten if `overwrite` is set.
    pub fn job_control_setenv<N: AsRef<OsStr>, V: AsRef<OsStr>>(
        &self,
        name: N,
        value: V,
        overwrite: bool,
    ) -> Result<(), SpankError> {
        self.do_setenv(name, value, overwrite, bindings::spank_setenv)
    }

    /// Sets the environment variable `name` in the job's environment to the provided `value`.
    ///
    /// Existing values will be overwritten if `overwrite` is set.
    /// This function can only be called from slurmstepd (remote context)
    pub fn setenv<N: AsRef<OsStr>, V: AsRef<OsStr>>(
        &self,
        name: N,
        value: V,
        overwrite: bool,
    ) -> Result<(), SpankError> {
        self.do_setenv(name, value, overwrite, bindings::spank_setenv)
    }

    fn do_unsetenv<N: AsRef<OsStr>>(
        &self,
        name: N,
        spank_fn: unsafe extern "C" fn(bindings::spank_t, *const c_char) -> bindings::spank_err_t,
    ) -> Result<(), SpankError> {
        let c_name = CString::new(name.as_ref().as_bytes()).map_err(|_| SpankError::BadArg)?;

        match unsafe { spank_fn(self.spank, c_name.as_ptr()) } {
            bindings::spank_err_ESPANK_SUCCESS => Ok(()),
            e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
        }
    }

    /// Unsets the environment variable `name` in the job's environment.
    ///
    /// This function can only be called from slurmstepd (remote context)
    pub fn unsetenv<N: AsRef<OsStr>>(&self, name: N) -> Result<(), SpankError> {
        self.do_unsetenv(name, bindings::spank_unsetenv)
    }

    /// Unsets the environment variable `name` in the job's control environment.
    ///
    /// This function can only be called from srun (local context)
    pub fn job_control_unsetenv<N: AsRef<OsStr>>(&self, name: N) -> Result<(), SpankError> {
        self.do_unsetenv(name, bindings::spank_job_control_unsetenv)
    }

    fn getopt_os(&self, name: &str) -> Result<Option<OsString>, SpankError> {
        let name_c = if let Ok(n) = CString::new(name) {
            n
        } else {
            return Ok(None);
        };

        let mut c_spank_opt = bindings::spank_option {
            name: name_c.as_ptr(),
            has_arg: 1,
            cb: None,
            usage: ptr::null(),
            arginfo: ptr::null(),
            val: 0,
        };

        let mut optarg: *mut c_char = ptr::null_mut();

        match unsafe { bindings::spank_option_getopt(self.spank, &mut c_spank_opt, &mut optarg) } {
            bindings::spank_err_ESPANK_SUCCESS => {
                if !optarg.is_null() {
                    Ok(Some(
                        OsStr::from_bytes(unsafe { CStr::from_ptr(optarg) }.to_bytes())
                            .to_os_string(),
                    ))
                } else {
                    Err(SpankError::Generic)
                }
            }
            bindings::spank_err_ESPANK_ERROR => Ok(None),
            e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
        }
    }

    // XXX: Unfortunately, according to the documentation, there are some
    // contexts where you can only use callbacks (init_post_opt) and others
    // where you can only use getopt (prolog/epilog). This is an
    // attempt at providing a uniform interface by caching callbacks or calls
    // to getopt which feels quite hackish. We should try to find a cleaner interface.
    fn update_cache_for_prolog(&mut self, name: &str) {
        if self.context() == Ok(Context::JobScript) {
            if let Ok(value_opt) = self.getopt_os(name) {
                self.opt_cache
                    .values
                    .insert(name.to_string(), vec![value_opt]);
            }
        }
    }

    /// Gets the value provided for the option `name`. Returns None if no value was set for this
    /// option. If the option was specified multiple times, it returns
    /// the last value provided.
    ///
    /// *NOTE*: In remote context, use `get_option_values` to
    /// access all values
    pub fn get_option_value_os(&mut self, name: &str) -> Option<&OsStr> {
        self.update_cache_for_prolog(name);

        if let Some(values) = self.opt_cache.values.get(name) {
            if let Some(value) = values.get(0) {
                return value.as_deref();
            } else {
                return None;
            }
        }

        None
    }

    pub fn get_option_value_lossy(&mut self, name: &str) -> Option<Cow<'_, str>> {
        self.get_option_value_os(name)
            .map(|value| value.to_string_lossy())
    }

    // TODO Fix error types
    pub fn get_option_value(&mut self, name: &str) -> Result<&str, SpankError> {
        self.get_option_value_os(name)
            .ok_or(SpankError::Generic)
            .and_then(|value| value.to_str().ok_or(SpankError::Generic))
    }

    /// Gets the list of values provided for the option `name`. Returns None if no value was set for this
    /// option.
    // TODO: Implement for prolog/epilog
    pub fn get_option_values_os(&mut self, name: &str) -> Option<Vec<&OsStr>> {
        self.update_cache_for_prolog(name);

        if let Some(values) = self.opt_cache.values.get(name) {
            values
                .iter()
                .map(|opt| opt.as_deref().ok_or(()))
                .collect::<Result<Vec<&OsStr>, ()>>()
                .ok()
        } else {
            None
        }
    }

    pub fn get_option_values_lossy(&mut self, name: &str) -> Option<Vec<Cow<'_, str>>> {
        self.get_option_values_os(name)
            .map(|values| values.iter().map(|opt| opt.to_string_lossy()).collect())
    }

    // TODO Fix error types
    pub fn get_option_values(&mut self, name: &str) -> Result<Vec<&str>, SpankError> {
        self.update_cache_for_prolog(name);

        self.get_option_values_os(name)
            .ok_or(SpankError::Generic)
            .and_then(|values| {
                values
                    .iter()
                    .map(|opt| opt.to_str().ok_or(SpankError::Generic))
                    .collect()
            })
    }

    /// Returns how many times an option was set
    ///
    // TODO: Implement for prolog/epilog
    pub fn get_option_count(&mut self, name: &str) -> usize {
        if self.context() == Ok(Context::JobScript) {
            if let Ok(value_opt) = self.getopt_os(name) {
                self.opt_cache
                    .values
                    .insert(name.to_string(), vec![value_opt]);
            }
        }

        if let Some(values) = self.opt_cache.values.get(name) {
            values.len()
        } else {
            0
        }
    }

    spank_item_getter!(job_gid, SpankItem::JobGid, gid_t);
    spank_item_getter!(job_uid, SpankItem::JobUid, uid_t);
    spank_item_getter!(job_id, SpankItem::JobId, u32);
    spank_item_getter!(job_stepid, SpankItem::JobStepid, u32);
    spank_item_getter!(job_nnodes, SpankItem::JobNnodes, u32);
    spank_item_getter!(job_nodeid, SpankItem::JobNodeid, u32);
    spank_item_getter!(job_local_task_count, SpankItem::JobLocalTaskCount, u32);
    spank_item_getter!(job_total_task_count, SpankItem::JobTotalTaskCount, u32);
    spank_item_getter!(job_ncpus, SpankItem::JobNcpus, u16);

    fn job_argv_c(&self) -> Result<(usize, *const *const c_char), SpankError> {
        let mut argc: c_int = 0;
        let mut argv: *const *const c_char = ptr::null_mut();

        let argc_ptr: *mut c_int = &mut argc;
        let argv_ptr: *mut *const *const c_char = &mut argv;

        match unsafe {
            bindings::spank_get_item(self.spank, SpankItem::JobArgv.into(), argc_ptr, argv_ptr)
        } {
            bindings::spank_err_ESPANK_SUCCESS => Ok((argc as usize, argv)),
            e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
        }
    }

    pub fn job_argv(&self) -> Result<Vec<&str>, SpankError> {
        self.job_argv_c()
            .and_then(|(argc, argv)| self.argv_to_vec(argc, argv))
    }

    pub fn job_argv_os(&self) -> Result<Vec<&OsStr>, SpankError> {
        self.job_argv_c()
            .and_then(|(argc, argv)| Ok(self.argv_to_vec_os(argc, argv)))
    }

    fn job_env_c(&self) -> Result<(usize, *const *const c_char), SpankError> {
        let mut argv: *const *const c_char = ptr::null_mut();
        let argv_ptr: *mut *const *const c_char = &mut argv;

        match unsafe { bindings::spank_get_item(self.spank, SpankItem::JobEnv.into(), argv_ptr) } {
            bindings::spank_err_ESPANK_SUCCESS => {
                if argv.is_null() {
                    return Err(SpankError::Generic);
                }
                let mut argc: isize = 0;
                while !unsafe { *argv.offset(argc as isize) }.is_null() {
                    argc += 1;
                }
                Ok((argc as usize, argv))
            }
            e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
        }
    }

    pub fn job_env(&self) -> Result<Vec<&str>, SpankError> {
        self.job_env_c()
            .and_then(|(argc, argv)| self.argv_to_vec(argc, argv))
    }

    pub fn job_env_os(&self) -> Result<Vec<&OsStr>, SpankError> {
        self.job_env_c()
            .and_then(|(argc, argv)| Ok(self.argv_to_vec_os(argc, argv)))
    }

    spank_item_getter!(task_id, SpankItem::TaskId, c_int);
    spank_item_getter!(task_global_id, SpankItem::TaskGlobalId, u32);
    spank_item_getter!(task_exit_status, SpankItem::TaskExitStatus, c_int);
    spank_item_getter!(task_pid, SpankItem::TaskPid, pid_t);
    spank_item_getter!(
        pid_to_global_id,
        SpankItem::JobPidToGlobalId,
        pid,
        pid_t,
        u32
    );
    spank_item_getter!(pid_to_local_id, SpankItem::JobPidToLocalId, pid, pid_t, u32);
    spank_item_getter!(
        local_to_global_id,
        SpankItem::JobLocalToGlobalId,
        local_id,
        u32,
        u32
    );
    spank_item_getter!(
        global_to_local_id,
        SpankItem::JobGlobalToLocalId,
        global_id,
        u32,
        u32
    );

    pub fn job_supplmentary_gids(&self) -> Result<Vec<gid_t>, SpankError> {
        let mut gidc: c_int = 0;
        let mut gidv: *const gid_t = ptr::null_mut();

        let gidc_ptr: *mut c_int = &mut gidc;
        let gidv_ptr: *mut *const gid_t = &mut gidv;

        match unsafe {
            bindings::spank_get_item(
                self.spank,
                SpankItem::JobSupplementaryGids.into(),
                gidv_ptr,
                gidc_ptr,
            )
        } {
            bindings::spank_err_ESPANK_SUCCESS => {
                Ok(unsafe { slice::from_raw_parts(gidv, gidc as usize) }
                    .iter()
                    .map(|&gid| gid)
                    .collect::<Vec<gid_t>>())
            }
            e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
        }
    }

    spank_item_getter!(slurm_version, SpankItem::SlurmVersion, &str);
    spank_item_getter!(slurm_version_major, SpankItem::SlurmVersionMajor, &str);
    spank_item_getter!(slurm_version_minor, SpankItem::SlurmVersionMinor, &str);
    spank_item_getter!(slurm_version_micro, SpankItem::SlurmVersionMicro, &str);
    spank_item_getter!(step_cpus_per_task, SpankItem::StepCpusPerTask, u64);
    spank_item_getter!(job_alloc_cores, SpankItem::JobAllocCores, u64);
    spank_item_getter!(job_alloc_mem, SpankItem::JobAllocMem, u64);
    spank_item_getter!(step_alloc_cores, SpankItem::StepAllocCores, u64);
    spank_item_getter!(step_alloc_mem, SpankItem::StepAllocMem, u64);
    spank_item_getter!(slurm_restart_count, SpankItem::SlurmRestartCount, u32);
    spank_item_getter!(job_array_id, SpankItem::JobArrayId, u32);
    spank_item_getter!(job_array_task_id, SpankItem::JobArrayTaskId, u32);
}

fn cstring_escape_null(msg: &str) -> CString {
    // XXX: We can't deal with NULL characters when passing strings to slurm log
    // functions, but how do we expect a plugin author to handle the error if we
    // returned one ? We assume they would prefer that we render them as a 0 in
    // the logs instead.
    let c_safe_msg = msg.split('\0').collect::<Vec<&str>>().join("0");

    // Should never panic as we made sure there is no NULL chars
    CString::new(&c_safe_msg as &str).unwrap()
}

pub enum LogLevel {
    Error,
    Info,
    Verbose,
    Debug,
    Debug2,
    Debug3,
}

pub fn spank_log(level: LogLevel, msg: &str) {
    let c_msg = cstring_escape_null(msg);

    match level {
        LogLevel::Error => unsafe { bindings::slurm_error(c_str!("%s").as_ptr(), c_msg.as_ptr()) },
        LogLevel::Info => unsafe { bindings::slurm_info(c_str!("%s").as_ptr(), c_msg.as_ptr()) },
        LogLevel::Verbose => unsafe {
            bindings::slurm_verbose(c_str!("%s").as_ptr(), c_msg.as_ptr())
        },
        LogLevel::Debug => unsafe { bindings::slurm_debug(c_str!("%s").as_ptr(), c_msg.as_ptr()) },
        LogLevel::Debug2 => unsafe {
            bindings::slurm_debug2(c_str!("%s").as_ptr(), c_msg.as_ptr())
        },
        LogLevel::Debug3 => unsafe {
            bindings::slurm_debug3(c_str!("%s").as_ptr(), c_msg.as_ptr())
        },
    }
}

#[no_mangle]
pub extern "C" fn spank_opt_callback(val: c_int, optarg: *const c_char, _remote: c_int) -> c_int {
    let mut opt_cache = match OPTIONS.lock() {
        Ok(opt_cache) => opt_cache,

        Err(e) => {
            spank_log(
                LogLevel::Error,
                &format!("Internal spank plugin error: {}", e),
            );
            return bindings::spank_err_ESPANK_ERROR as c_int;
        }
    };

    let name = match opt_cache.options.get(val as usize) {
        Some(name) => name.clone(),
        None => {
            spank_log(
                LogLevel::Error,
                &format!(
                    "Internal spank plugin error: received unexpected option callback {}",
                    val
                ),
            );
            return bindings::spank_err_ESPANK_ERROR as c_int;
        }
    };

    let optarg = if optarg == ptr::null() {
        None
    } else {
        Some(OsStr::from_bytes(unsafe { CStr::from_ptr(optarg) }.to_bytes()).to_os_string())
    };

    opt_cache.values.entry(name).or_insert(vec![]).push(optarg);

    bindings::spank_err_ESPANK_SUCCESS as c_int
}

lazy_static! {
    // XXX Slurm should never call us from multiple threads so we
    // could probably use a RefCell or UnsafeCell and force it Sync
    static ref OPTIONS: Mutex<OptionCache> = Mutex::new(OptionCache::default());
}

#[macro_export]
macro_rules! SPANK_PLUGIN {
    ($spank_name:literal, $spank_version:literal, $spank_ty:ty) => {
        #[no_mangle]
        pub static plugin_name: StaticCStr =
            StaticCStr({ concat_bytes!($spank_name, b"\0") } as *const u8);
        #[no_mangle]
        pub static plugin_type: StaticCStr = StaticCStr(b"spank" as *const u8);
        // #[no_mangle]
        // pub static plugin_version: c_int = $spank_version;

        lazy_static! {
            // XXX Slurm should never call us from multiple threads so we
            // could probably use a RefCell or UnsafeCell and force it Sync
            static ref PLUGIN: Mutex<$spank_ty> = Mutex::new(<$spank_ty>::default());

        }

        fn _check_spank_trait<T: Plugin>() {}
        fn _t() {
            _check_spank_trait::<$spank_ty>()
        }

        macro_rules! spank_hook {
            ($c_spank_cb:ident, $rust_spank_cb:ident) => {
                #[no_mangle]
                #[doc(hidden)]
                pub extern "C" fn $c_spank_cb(
                    spank: bindings::spank_t,
                    ac: c_int,
                    argv: *const *const c_char,
                ) -> c_int {
                    let res = catch_unwind(|| {
                        //TODO: factor this op
                        let mut plugin = match PLUGIN.lock() {
                            Ok(plugin) => plugin,
                            Err(e) => {
                                spank_log(
                                    LogLevel::Error,
                                    &format!("Internal spank plugin error: {}", e),
                                );
                                return bindings::spank_err_ESPANK_ERROR as c_int;
                            }
                        };

                        let mut opt_cache = match OPTIONS.lock() {
                            Ok(cache) => cache,
                            Err(e) => {
                                spank_log(
                                    LogLevel::Error,
                                    &format!("Internal spank plugin error: {}", e),
                                );
                                return bindings::spank_err_ESPANK_ERROR as c_int;
                            }
                        };

                        match plugin.$rust_spank_cb(&mut SpankHandle {
                            spank: spank,
                            opt_cache: &mut opt_cache,
                            argc: ac,
                            argv: argv,
                        }) {
                            Ok(()) => bindings::spank_err_ESPANK_SUCCESS as c_int,
                            Err(err) => -(err as c_int),
                        }
                    });

                    match res {
                        Ok(e) => e,
                        Err(_) => bindings::spank_err_ESPANK_ERROR as c_int,
                    }
                }
            };
        }

        spank_hook!(slurm_spank_init, init);
        spank_hook!(slurm_spank_job_prolog, job_prolog);
        spank_hook!(slurm_spank_init_post_opt, init_post_opt);
        spank_hook!(slurm_spank_local_user_init, local_user_init);
        spank_hook!(slurm_spank_user_init, user_init);
        spank_hook!(slurm_spank_task_init_privileged, task_init_privileged);
        spank_hook!(slurm_spank_task_init, task_init);
        spank_hook!(slurm_spank_task_post_fork, task_post_fork);
        spank_hook!(slurm_spank_task_exit, task_exit);
        spank_hook!(slurm_spank_job_epilog, job_epilog);
        spank_hook!(slurm_spank_slurmd_exit, slurmd_exit);
        spank_hook!(slurm_spank_exit, exit);
    };
}

#[allow(unused_variables)]
pub trait Plugin {
    fn init(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn job_prolog(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn init_post_opt(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn local_user_init(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn user_init(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn task_init_privileged(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn task_init(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn task_post_fork(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn task_exit(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn job_epilog(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn slurmd_exit(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn exit(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
}
impl Error for SpankError {}

impl fmt::Display for SpankError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cerr = unsafe { CStr::from_ptr(bindings::spank_strerror(*self as u32)) };

        if let Ok(err) = cerr.to_str() {
            write!(f, "{}", err)
        } else {
            write!(f, "Invalid Error")
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, IntoPrimitive)]
#[repr(u32)] // FIXME: force the type generated by bindgen
enum SpankItem {
    JobGid = bindings::spank_item_S_JOB_GID,
    JobUid = bindings::spank_item_S_JOB_UID,
    JobId = bindings::spank_item_S_JOB_ID,
    JobStepid = bindings::spank_item_S_JOB_STEPID,
    JobNnodes = bindings::spank_item_S_JOB_NNODES,
    JobNodeid = bindings::spank_item_S_JOB_NODEID,
    JobLocalTaskCount = bindings::spank_item_S_JOB_LOCAL_TASK_COUNT,
    JobTotalTaskCount = bindings::spank_item_S_JOB_TOTAL_TASK_COUNT,
    JobNcpus = bindings::spank_item_S_JOB_NCPUS,
    JobArgv = bindings::spank_item_S_JOB_ARGV,
    JobEnv = bindings::spank_item_S_JOB_ENV,
    TaskId = bindings::spank_item_S_TASK_ID,
    TaskGlobalId = bindings::spank_item_S_TASK_GLOBAL_ID,
    TaskExitStatus = bindings::spank_item_S_TASK_EXIT_STATUS,
    TaskPid = bindings::spank_item_S_TASK_PID,
    JobPidToGlobalId = bindings::spank_item_S_JOB_PID_TO_GLOBAL_ID,
    JobPidToLocalId = bindings::spank_item_S_JOB_PID_TO_LOCAL_ID,
    JobLocalToGlobalId = bindings::spank_item_S_JOB_LOCAL_TO_GLOBAL_ID,
    JobGlobalToLocalId = bindings::spank_item_S_JOB_GLOBAL_TO_LOCAL_ID,
    JobSupplementaryGids = bindings::spank_item_S_JOB_SUPPLEMENTARY_GIDS,
    SlurmVersion = bindings::spank_item_S_SLURM_VERSION,
    SlurmVersionMajor = bindings::spank_item_S_SLURM_VERSION_MAJOR,
    SlurmVersionMinor = bindings::spank_item_S_SLURM_VERSION_MINOR,
    SlurmVersionMicro = bindings::spank_item_S_SLURM_VERSION_MICRO,
    StepCpusPerTask = bindings::spank_item_S_STEP_CPUS_PER_TASK,
    JobAllocCores = bindings::spank_item_S_JOB_ALLOC_CORES,
    JobAllocMem = bindings::spank_item_S_JOB_ALLOC_MEM,
    StepAllocCores = bindings::spank_item_S_STEP_ALLOC_CORES,
    StepAllocMem = bindings::spank_item_S_STEP_ALLOC_MEM,
    SlurmRestartCount = bindings::spank_item_S_SLURM_RESTART_COUNT,
    JobArrayId = bindings::spank_item_S_JOB_ARRAY_ID,
    JobArrayTaskId = bindings::spank_item_S_JOB_ARRAY_TASK_ID,
}

#[derive(Debug, Copy, Clone, PartialEq, IntoPrimitive, TryFromPrimitive)]
#[repr(u32)] // FIXME: force the type generated by bindgen
pub enum SpankError {
    Generic = bindings::spank_err_ESPANK_ERROR,
    BadArg = bindings::spank_err_ESPANK_BAD_ARG,
    NotTask = bindings::spank_err_ESPANK_NOT_TASK,
    EnvExists = bindings::spank_err_ESPANK_ENV_EXISTS,
    EnvNotExist = bindings::spank_err_ESPANK_ENV_NOEXIST,
    NoSpace = bindings::spank_err_ESPANK_NOSPACE,
    NotRemote = bindings::spank_err_ESPANK_NOT_REMOTE,
    NoExist = bindings::spank_err_ESPANK_NOEXIST,
    NotExecd = bindings::spank_err_ESPANK_NOT_EXECD,
    NotAvail = bindings::spank_err_ESPANK_NOT_AVAIL,
    NotLocal = bindings::spank_err_ESPANK_NOT_LOCAL,
}

#[derive(Debug, Copy, Clone, PartialEq, IntoPrimitive, TryFromPrimitive)]
#[repr(u32)] // FIXME: force the type generated by bindgen
pub enum Context {
    // We dont represent error here, as errors are better embedded in Results
    Local = bindings::spank_context_S_CTX_LOCAL,
    Remote = bindings::spank_context_S_CTX_REMOTE,
    Allocator = bindings::spank_context_S_CTX_ALLOCATOR,
    Slurmd = bindings::spank_context_S_CTX_SLURMD,
    JobScript = bindings::spank_context_S_CTX_JOB_SCRIPT,
}

pub struct SpankOption {
    name: String,
    arginfo: Option<String>,
    usage: Option<String>,
}

impl SpankOption {
    pub fn new(name: &str) -> Self {
        SpankOption {
            name: name.to_string(),
            arginfo: None,
            usage: None,
        }
    }
    pub fn usage(mut self, usage: &str) -> Self {
        self.usage = Some(usage.to_string());
        self
    }
    pub fn has_arg(mut self, arg_name: &str) -> Self {
        self.arginfo = Some(arg_name.to_string());
        self
    }
}

// Example below
#[derive(Default)]
struct TestSpank {
    data: u32,
}

impl Plugin for TestSpank {
    fn init(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        println!(
            "C'est l'initialisation et les arguments sont: {:?}",
            spank.plugin_argv()
        );
        println!("Context is {:?}", spank.context()?);
        self.data = 20;
        spank.register_option(SpankOption::new("opt_a").usage("c'est une option"))?;
        // Ok(vec![
        //     SpankOption::new("opt_a"),
        //     SpankOption::new("opt_b").has_arg("val"),
        // ])
        println!("{:?}", spank.job_env());
        Ok(())
    }
    fn task_init(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        spank_log(
            LogLevel::Error,
            &format!("{:?}", spank.job_supplmentary_gids()?),
        );
        Ok(())
    }
    fn task_exit(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        spank_log(
            LogLevel::Error,
            &format!("Job env task exit {:?}", spank.job_env()?),
        );
        spank_log(LogLevel::Error, "MySpank: task exited");
        Ok(())
    }
    fn exit(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        println!("Data is {:?}", self.data);
        println!("CB returned {:?}", spank.get_option_value("opt_a"));

        spank_log(LogLevel::Error, "Goodbye from spank logs\n");
        Ok(())
    }
}

SPANK_PLUGIN!(b"toto", 0x130502, TestSpank);
