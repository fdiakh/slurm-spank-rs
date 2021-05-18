//! Rust interface for writing Slurm SPANK Plugins

use byte_strings::{c_str, concat_bytes};
use lazy_static::lazy_static;
use libc::{gid_t, pid_t, uid_t};
use num_enum::{FromPrimitive, IntoPrimitive, TryFromPrimitive};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::error::Error;
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fmt;
use std::os::raw::{c_char, c_int};
use std::os::unix::ffi::OsStrExt;
use std::panic::catch_unwind;
use std::panic::UnwindSafe;
use std::sync::Mutex;
use std::{ptr, slice};

mod bindings;

/// This struct represents a handle to the Slurm interface exposed to SPANK
/// plugins. It provides methods to query Slurm from a plugin.
pub struct SpankHandle<'a> {
    spank: bindings::spank_t,
    opt_cache: &'a mut OptionCache,
    argc: c_int,
    argv: *const *const c_char,
    spank_opt_cb: unsafe extern "C" fn(c_int, *const c_char, c_int) -> c_int,
}

macro_rules! spank_item_getter {
    ($(#[$outer:meta])* $name:ident, $spank_item:path, $arg_name:ident, $arg_type:ty, $result_type:ty) => {
        $(#[$outer])*
        pub fn $name(&self, $arg_name: $arg_type) -> Result<$result_type, SpankError> {
            let mut res: $result_type = <$result_type>::default();
            let res_ptr: *mut $result_type = &mut res;
            match unsafe {
                bindings::spank_get_item(self.spank, $spank_item.into(), $arg_name, res_ptr)
            } {
                bindings::spank_err_ESPANK_SUCCESS => Ok(res),
                bindings::spank_err_ESPANK_NOEXIST => Err(SpankError::from_noexist($arg_name)),
                e => Err(SpankError::from_spank("spank_get_item", e)),
            }
        }
    };
    ($(#[$outer:meta])* $name:ident, $spank_item:path, &str) => {
        $(#[$outer])*
        pub fn $name(&self) -> Result<&str, SpankError> {
            let mut res: *const c_char = ptr::null_mut();
            let res_ptr: *mut *const c_char = &mut res;
            match unsafe { bindings::spank_get_item(self.spank, $spank_item.into(), res_ptr) } {
                bindings::spank_err_ESPANK_SUCCESS => {
                    if res.is_null() {
                        panic!("Received unexpected null pointer from spank_get_item")
                    } else {
                        let cstr = unsafe { CStr::from_ptr(res) };
                        cstr.to_str()
                            .map_err(|_| SpankError::Utf8Error(cstr.to_string_lossy().to_string()))
                    }
                }
                e => Err(SpankError::from_spank("spank_get_item", e)),
            }
        }
    };
    ($(#[$outer:meta])* $name:ident, $spank_item:path,$result_type:ty) => {
        $(#[$outer])*
        pub fn $name(&self) -> Result<$result_type, SpankError> {
            let mut res: $result_type = <$result_type>::default();
            let res_ptr: *mut $result_type = &mut res;
            match unsafe { bindings::spank_get_item(self.spank, $spank_item.into(), res_ptr) } {
                bindings::spank_err_ESPANK_SUCCESS => Ok(res),
                e => Err(SpankError::from_spank("spank_get_item", e)),
            }
        }
    };
}

fn os_value_to_lossy(value: Cow<'_, OsStr>) -> Cow<'_, str> {
    match value {
        Cow::Borrowed(value) => value.to_string_lossy(),
        Cow::Owned(value) => match value.into_string() {
            Ok(utf8_value) => Cow::from(utf8_value),
            Err(value) => Cow::from(value.to_string_lossy().into_owned()),
        },
    }
}
fn os_value_to_str(value: Cow<'_, OsStr>) -> Result<Cow<'_, str>, SpankError> {
    match value {
        Cow::Borrowed(value) => Ok(Cow::from(
            value.to_str().ok_or(SpankError::from_os_str(value))?,
        )),
        Cow::Owned(value) => {
            Ok(Cow::from(value.into_string().map_err(|value_err| {
                SpankError::from_os_str(&value_err)
            })?))
        }
    }
}

// XXX: Unfortunately, according to the documentation, there are some contexts
// where you can only use callbacks (init_post_opt) and others where you can
// only use getopt (prolog/epilog). This is an attempt at providing a uniform
// interface by caching callbacks or calls to getopt which feels quite hackish.
// We should try to find a cleaner interface.
#[derive(Default)]
struct OptionCache {
    options: Vec<String>,
    values: HashMap<String, Vec<Option<OsString>>>,
}

impl<'a> SpankHandle<'a> {
    /// Returns the context in which the calling plugin is loaded.
    pub fn context(&self) -> Result<Context, SpankError> {
        let ctx = unsafe { bindings::spank_context() };
        Context::try_from(ctx)
            .map_err(|_| SpankError::from_spank("spank_context", bindings::spank_err_ESPANK_ERROR))
    }

    /// Registers a plugin-provided option dynamically. This function is only
    /// valid when called from a plugin's `init()`, and must be guaranteed to be
    /// called in all contexts in which it is used (local, remote, allocator).
    pub fn register_option(&mut self, spank_opt: SpankOption) -> Result<(), SpankError> {
        let arginfo = match spank_opt.arginfo {
            None => None,
            Some(info) => Some(CString::new(&info as &str).or(Err(SpankError::from_str(&info)))?),
        };
        let name =
            CString::new(&spank_opt.name as &str).or(Err(SpankError::from_str(&spank_opt.name)))?;
        let usage = match spank_opt.usage {
            None => None,
            Some(usage) => {
                Some(CString::new(&usage as &str).or(Err(SpankError::from_str(&usage)))?)
            }
        };

        let mut c_spank_opt = bindings::spank_option {
            name: name.as_ptr(),
            has_arg: arginfo.is_some() as i32,
            cb: Some(self.spank_opt_cb),
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
                .expect("Argument table overflow"),
        };

        match unsafe { bindings::spank_option_register(self.spank, &mut c_spank_opt) } {
            bindings::spank_err_ESPANK_SUCCESS => {
                self.opt_cache.options.push(spank_opt.name);
                Ok(())
            }
            e => Err(SpankError::from_spank("spank_option_register", e)),
        }
    }

    /// Returns the list of arguments configured in the `plugstack.conf` file
    /// for this plugin
    pub fn plugin_argv(&self) -> Result<Vec<&str>, SpankError> {
        self.argv_to_vec(self.argc as usize, self.argv)
    }

    fn argv_to_vec(
        &self,
        argc: usize,
        argv: *const *const c_char,
    ) -> Result<Vec<&str>, SpankError> {
        unsafe { slice::from_raw_parts(argv, argc) }
            .iter()
            .map(|&arg| {
                let cstr = unsafe { CStr::from_ptr(arg) };
                cstr.to_str().map_err(|_| SpankError::from_cstr(cstr))
            })
            .collect::<Result<Vec<_>, _>>()
    }

    fn argv_to_vec_os(&self, argc: usize, argv: *const *const c_char) -> Vec<&OsStr> {
        unsafe { slice::from_raw_parts(argv, argc) }
            .iter()
            .map(|&arg| OsStr::from_bytes(unsafe { CStr::from_ptr(arg) }.to_bytes()))
            .collect()
    }

    ///  Retrieves the environment variable `name` from the job's environment as
    ///  a String
    ///
    ///  This function returns Ok(none) if the environment variable is not set.
    ///  It returns an error if the value is not a valid UTF-8 string or if
    ///  called outside of remote context. To access job environment variables
    ///  from local context, use std::env directly
    pub fn getenv<N: AsRef<OsStr>>(&self, name: N) -> Result<Option<&str>, SpankError> {
        match self.do_getenv_os(name, bindings::spank_getenv)? {
            None => Ok(None),
            Some(env) => Ok(Some(env.to_str().ok_or(SpankError::from_os_str(env))?)),
        }
    }

    ///  Retrieves the environment variable `name` from the job's environment as
    ///  a lossy String
    ///
    ///  If the value contains invalid UTF-8 code points, those invalid points
    ///  will be replaced with � (U+FFFD). This function returns Ok(none) if the
    ///  environment variable is not set. It returns an error if called outside
    ///  of remote conext. To access job environment variables from local
    ///  context, use std::env directly
    pub fn getenv_lossy<N: AsRef<OsStr>>(
        &self,
        name: N,
    ) -> Result<Option<Cow<'_, str>>, SpankError> {
        self.do_getenv_os(name, bindings::spank_getenv)
            .and_then(|env| Ok(env.map(|s| s.to_string_lossy())))
    }

    ///  Retrieves the environment variable `name` from the job's environment as
    ///  an OsString
    ///
    ///  The return value is an OsString which can hold arbitrary sequences of
    ///  bytes on Unix-like systems. This function returns Ok(none) if the
    ///  environment variable is not set. It returns an error if called outside
    ///  of remote conext. To access job environment variables from local
    ///  context, use std::env directly
    pub fn getenv_os<N: AsRef<OsStr>>(&self, name: N) -> Result<Option<&OsStr>, SpankError> {
        self.do_getenv_os(name, bindings::spank_getenv)
    }

    ///  Retrieves the environment variable `name` from the job's control
    ///  environment as a String
    ///
    ///  This function returns Ok(none) if the environment variable is not set.
    ///  It returns an error if the value is not a valid UTF-8 string or if
    ///  called outside of local context. To access job control environment
    ///  variables from remote context, use std::env directly.
    pub fn job_control_getenv<N: AsRef<OsStr>>(&self, name: N) -> Result<Option<&str>, SpankError> {
        match self.do_getenv_os(name, bindings::spank_job_control_getenv)? {
            None => Ok(None),
            Some(env) => Ok(Some(env.to_str().ok_or(SpankError::from_os_str(env))?)),
        }
    }

    ///  Retrieves the environment variable `name` from the job's control
    ///  environment as a lossy String
    ///
    ///  If the value contains invalid UTF-8 code points, those invalid points
    ///  will be replaced with � (U+FFFD). This function returns Ok(none) if the
    ///  environment variable is not set. It returns an error if called outside
    ///  of local context. To access job control environment variables from
    ///  remote context, use std::env directly.
    pub fn job_control_getenv_lossy<N: AsRef<OsStr>>(
        &self,
        name: N,
    ) -> Result<Option<Cow<'_, str>>, SpankError> {
        self.do_getenv_os(name, bindings::spank_job_control_getenv)
            .and_then(|env| Ok(env.map(|s| s.to_string_lossy())))
    }

    ///  Retrieves the environment variable `name` from the job's control
    ///  environment as an OsString
    ///
    ///  The return value is an OsString which can hold arbitrary sequences of
    ///  bytes on Unix-like systems. This function returns Ok(none) if the
    ///  environment variable is not set. It returns an error if called outside
    ///  of local context. To access job control environment variables from
    ///  remote context, use std::env directly.
    pub fn job_control_getenv_os<N: AsRef<OsStr>>(
        &self,
        name: N,
    ) -> Result<Option<&OsStr>, SpankError> {
        self.do_getenv_os(name, bindings::spank_job_control_getenv)
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
    ) -> Result<Option<&OsStr>, SpankError> {
        let mut max_size = 4096;
        let c_name = CString::new(name.as_ref().as_bytes())
            .map_err(|_| SpankError::from_str(&name.as_ref().to_string_lossy()))?;
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
                    return Ok(Some(OsStr::from_bytes(
                        unsafe { CStr::from_ptr(buffer_ptr) }.to_bytes(),
                    )))
                }
                bindings::spank_err_ESPANK_ENV_NOEXIST => return Ok(None),
                e => return Err(SpankError::from_spank("spank_getenv", e)),
            }
        }
    }

    /// Sets the environment variable `name` in the job's environment to the
    /// provided `value`.
    ///
    /// Existing values will be overwritten if `overwrite` is set. This function
    /// will return an error if called outside of remote context. To access job
    /// environment variables from local context, use std::env directly.
    pub fn setenv<N: AsRef<OsStr>, V: AsRef<OsStr>>(
        &self,
        name: N,
        value: V,
        overwrite: bool,
    ) -> Result<(), SpankError> {
        self.do_setenv(name, value, overwrite, bindings::spank_setenv)
    }

    /// Sets the environment variable `name` in the job's control environment to
    /// the provided `value`.
    ///
    /// Existing values will be overwritten if `overwrite` is set. This function
    /// will return an error if called outside of local context. To access job
    /// control environment variables from remote context, use std::env directly.
    pub fn job_control_setenv<N: AsRef<OsStr>, V: AsRef<OsStr>>(
        &self,
        name: N,
        value: V,
        overwrite: bool,
    ) -> Result<(), SpankError> {
        self.do_setenv(name, value, overwrite, bindings::spank_setenv)
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
        let c_name = CString::new(name.as_ref().as_bytes())
            .map_err(|_| SpankError::from_os_str(name.as_ref()))?;
        let c_value = CString::new(value.as_ref().as_bytes())
            .map_err(|_| SpankError::from_os_str(value.as_ref()))?;

        match unsafe {
            spank_fn(
                self.spank,
                c_name.as_ptr(),
                c_value.as_ptr(),
                overwrite as c_int,
            )
        } {
            bindings::spank_err_ESPANK_SUCCESS => Ok(()),
            bindings::spank_err_ESPANK_ENV_EXISTS => Err(SpankError::EnvExists(
                name.as_ref().to_string_lossy().to_string(),
            )),
            e => Err(SpankError::from_spank("spank_setenv", e)),
        }
    }

    /// Unsets the environment variable `name` in the job's environment.
    ///
    /// This function is a no-op if the variable is already unset. It will return an
    /// error if called outside of remote context. To access the job variables
    /// from local context, use std::env directly.
    pub fn unsetenv<N: AsRef<OsStr>>(&self, name: N) -> Result<(), SpankError> {
        self.do_unsetenv(name, bindings::spank_unsetenv)
    }

    /// Unsets the environment variable `name` in the job's control environment.
    ///
    /// This function is a no-op if the variable is already unset. It will
    /// return an error if called outside of local context. To access job
    /// control environment variables from remote context, use std::env
    /// directly.
    pub fn job_control_unsetenv<N: AsRef<OsStr>>(&self, name: N) -> Result<(), SpankError> {
        self.do_unsetenv(name, bindings::spank_job_control_unsetenv)
    }

    fn do_unsetenv<N: AsRef<OsStr>>(
        &self,
        name: N,
        spank_fn: unsafe extern "C" fn(bindings::spank_t, *const c_char) -> bindings::spank_err_t,
    ) -> Result<(), SpankError> {
        let c_name = CString::new(name.as_ref().as_bytes())
            .map_err(|_| SpankError::from_os_str(name.as_ref()))?;

        match unsafe { spank_fn(self.spank, c_name.as_ptr()) } {
            bindings::spank_err_ESPANK_SUCCESS => Ok(()),
            e => Err(SpankError::from_spank("spank_unsetenv", e)),
        }
    }

    fn getopt_os(&self, name: &str) -> Result<Option<OsString>, SpankError> {
        let name_c = if let Ok(n) = CString::new(name) {
            n
        } else {
            return Err(SpankError::from_str(name));
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
                    Ok(None)
                }
            }
            e => Err(SpankError::from_spank("spank_option_getopt", e)),
        }
    }

    /// Returns the value set for the option `name` as an OsString
    ///
    /// If the option was specified multiple times, it returns the last value
    /// provided. Outside of job_script context, use get_option_values to access
    /// all values.
    ///
    /// *WARNING*: If options have not yet been processed (e.g in init callbacks
    /// or all slurmd contexts), this function will always return None.
    ///
    /// *WARNING*: This function always returns None for options which don't
    /// take values (flag options created without has_arg()) no matter whether
    /// they were used or not. To check whether a flag was set, use
    /// get_option_count.
    pub fn get_option_value_os(&self, name: &str) -> Option<Cow<'_, OsStr>> {
        match self.context() {
            Ok(Context::JobScript) => self
                .getopt_os(name)
                .ok() // We made sure call from the correct context
                .map(|opt| opt.map(|value| Cow::from(value)))
                .unwrap_or(None),
            _ => {
                if let Some(values) = self.opt_cache.values.get(name) {
                    if let Some(Some(ref value)) = values.last() {
                        Some(Cow::from(value))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        }
    }

    /// Returns the value set for the option `name` as a lossy String
    ///
    /// If the value contains invalid UTF-8 code points, those invalid points
    /// will be replaced with � (U+FFFD). If the option was specified multiple
    /// times, this function returns the last value provided. Outside of
    /// job_script context, use get_option_values to access all values.
    ///
    /// *WARNING*: If options have not yet been processed (e.g in init callbacks
    /// or all slurmd contexts), this function will always return None.
    ///
    /// *WARNING*: This function always returns None for options which don't
    /// take values (flag options created without has_arg()) no matter whether
    /// they were used or not. To check whether a flag was set, use
    /// get_option_count.
    pub fn get_option_value_lossy(&self, name: &str) -> Option<Cow<'_, str>> {
        self.get_option_value_os(name)
            .map(|value| os_value_to_lossy(value))
    }

    /// Returns the value set for the option `name` as a String
    ///
    /// An error is returned if the value cannot be converted to a String. If
    /// the option was specified multiple times, it returns the last value
    /// provided. Outside of job_script context, use get_option_values to access
    /// all values.
    ///
    /// *WARNING*: If options have not yet been processed (e.g in init callbacks
    /// or all slurmd contexts), this function will always return None.
    ///
    /// *WARNING*: This function always returns None for options which don't
    /// take values (flag options created without has_arg()) no matter whether
    /// they were used or not. To check whether a flag was set, use
    /// get_option_count.
    pub fn get_option_value(&self, name: &str) -> Result<Option<Cow<'_, str>>, SpankError> {
        match self.get_option_value_os(name) {
            Some(val) => Ok(Some(os_value_to_str(val)?)),
            None => Ok(None),
        }
    }

    /// Returns a list of values set for the option `name` as an OsString
    ///
    /// *WARNING*: In job_script context, only the last value is available.
    ///
    /// *WARNING*: If options have not yet been processed (e.g in init callbacks
    /// or all slurmd contexts), this function will always return None.
    ///
    /// *WARNING*: This function always returns None for options which don't
    /// take values (flag options created without has_arg()) no matter whether
    /// they were used or not. To check whether a flag was set, use
    /// get_option_count.
    pub fn get_option_values_os(&self, name: &str) -> Option<Vec<Cow<'_, OsStr>>> {
        match self.context() {
            Ok(Context::JobScript) => self.get_option_value_os(name).map(|value| vec![value]),
            _ => {
                if let Some(values) = self.opt_cache.values.get(name) {
                    values
                        .iter()
                        .map(|value| value.as_deref().ok_or(()).map(|opt| Cow::from(opt)))
                        .collect::<Result<Vec<Cow<'_, OsStr>>, ()>>()
                        .ok()
                } else {
                    None
                }
            }
        }
    }

    /// Returns a list of values set for the option `name` as a lossy String
    ///
    /// If the value contains invalid UTF-8 code points, those invalid points
    /// will be replaced with � (U+FFFD).
    ///
    /// *WARNING*: In job_script context, only the last value is available.
    ///
    /// *WARNING*: If options have not yet been processed (e.g in init callbacks
    /// or all slurmd contexts), this function will always return None.
    ///
    /// *WARNING*: This function always returns None for options which don't
    /// take values (flag options created without has_arg()) no matter whether
    /// they were used or not. To check whether a flag was set, use
    /// get_option_count.
    pub fn get_option_values_lossy(&self, name: &str) -> Option<Vec<Cow<'_, str>>> {
        self.get_option_values_os(name).map(|values| {
            values
                .into_iter()
                .map(|opt| os_value_to_lossy(opt))
                .collect()
        })
    }

    /// Returns a list of values set for the option `name` as a String
    ///
    /// An error is returned if the value cannot be converted to a String.
    ///
    /// *WARNING*: In job_script context, only the last value is available.
    ///
    /// *WARNING*: If options have not yet been processed (e.g in init callbacks
    /// or all slurmd contexts), this function will always return None.
    ///
    /// *WARNING*: This function always returns None for options which don't
    /// take values (flag options created without has_arg()) no matter whether
    /// they were used or not. To check whether a flag was set, use
    /// get_option_count().
    pub fn get_option_values(&self, name: &str) -> Result<Option<Vec<Cow<'_, str>>>, SpankError> {
        let values = self.get_option_values_os(name);
        match values {
            None => Ok(None),
            Some(values) => Ok(Some(
                values
                    .into_iter()
                    .map(|opt| os_value_to_str(opt))
                    .collect::<Result<Vec<_>, _>>()?,
            )),
        }
    }

    /// Returns how many times an option was set
    ///
    /// Use this function to process flag options.
    ///
    /// *WARNING*: In job_script context, this function can only return 0 or 1
    ///
    /// *WARNING*: If options have not yet been processed (e.g in init callbacks
    /// or all slurmd contexts), this function will always return 0.
    pub fn get_option_count(&mut self, name: &str) -> usize {
        match self.context() {
            Ok(Context::JobScript) => self.getopt_os(name).is_ok() as usize,
            _ => {
                if let Some(values) = self.opt_cache.values.get(name) {
                    values.len()
                } else {
                    0
                }
            }
        }
    }

    spank_item_getter!(
        /// Primary group id
        job_gid,
        SpankItem::JobGid,
        gid_t
    );
    spank_item_getter!(
        /// User id
        job_uid,
        SpankItem::JobUid,
        uid_t
    );
    spank_item_getter!(
        /// Slurm job id
        job_id,
        SpankItem::JobId,
        u32
    );
    spank_item_getter!(
        /// Slurm job step id
        job_stepid,
        SpankItem::JobStepid,
        u32
    );
    spank_item_getter!(
        /// Total number of nodes in job
        job_nnodes,
        SpankItem::JobNnodes,
        u32
    );
    spank_item_getter!(
        /// Relative id of this node
        job_nodeid,
        SpankItem::JobNodeid,
        u32
    );
    spank_item_getter!(
        /// Number of local tasks
        job_local_task_count,
        SpankItem::JobLocalTaskCount,
        u32
    );
    spank_item_getter!(
        /// Total number of tasks in job
        job_total_task_count,
        SpankItem::JobTotalTaskCount,
        u32
    );
    spank_item_getter!(
        /// Number of CPUs used by this job
        job_ncpus,
        SpankItem::JobNcpus,
        u16
    );

    /// Command args as Strings
    pub fn job_argv(&self) -> Result<Vec<&str>, SpankError> {
        self.job_argv_c()
            .and_then(|(argc, argv)| self.argv_to_vec(argc, argv))
    }

    /// Command args as OsStrings
    pub fn job_argv_os(&self) -> Result<Vec<&OsStr>, SpankError> {
        self.job_argv_c()
            .and_then(|(argc, argv)| Ok(self.argv_to_vec_os(argc, argv)))
    }

    fn job_argv_c(&self) -> Result<(usize, *const *const c_char), SpankError> {
        let mut argc: c_int = 0;
        let mut argv: *const *const c_char = ptr::null_mut();

        let argc_ptr: *mut c_int = &mut argc;
        let argv_ptr: *mut *const *const c_char = &mut argv;

        match unsafe {
            bindings::spank_get_item(self.spank, SpankItem::JobArgv.into(), argc_ptr, argv_ptr)
        } {
            bindings::spank_err_ESPANK_SUCCESS => {
                if argv.is_null() {
                    panic!("spank_get_item returned unexpected NULL ptr");
                }
                Ok((argc as usize, argv))
            }
            e => Err(SpankError::from_spank("sapnk_get_item", e)),
        }
    }

    /// Job env array as Strings
    pub fn job_env(&self) -> Result<Vec<&str>, SpankError> {
        self.job_env_c()
            .and_then(|(argc, argv)| self.argv_to_vec(argc, argv))
    }

    /// Job env array as OsStrings
    pub fn job_env_os(&self) -> Result<Vec<&OsStr>, SpankError> {
        self.job_env_c()
            .and_then(|(argc, argv)| Ok(self.argv_to_vec_os(argc, argv)))
    }

    fn job_env_c(&self) -> Result<(usize, *const *const c_char), SpankError> {
        let mut envv: *const *const c_char = ptr::null_mut();

        match unsafe { bindings::spank_get_item(self.spank, SpankItem::JobEnv.into(), &mut envv) } {
            bindings::spank_err_ESPANK_SUCCESS => {
                if envv.is_null() {
                    panic!("spank_get_item returned unexpected NULL ptr")
                }
                let mut argc: isize = 0;
                while !unsafe { *envv.offset(argc as isize) }.is_null() {
                    argc += 1;
                }
                Ok((argc as usize, envv))
            }
            e => Err(SpankError::from_spank("spank_get_item", e)),
        }
    }

    spank_item_getter!(
        /// Local task id
        task_id,
        SpankItem::TaskId,
        c_int
    );

    spank_item_getter!(
        /// Global task id
        task_global_id,
        SpankItem::TaskGlobalId,
        u32
    );

    spank_item_getter!(
        /// Exit status of task if exited
        task_exit_status,
        SpankItem::TaskExitStatus,
        c_int
    );

    spank_item_getter!(
        /// Task pid
        task_pid,
        SpankItem::TaskPid,
        pid_t
    );
    spank_item_getter!(
        /// Global task id from pid
        pid_to_global_id,
        SpankItem::JobPidToGlobalId,
        pid,
        pid_t,
        u32
    );
    spank_item_getter!(
        /// Local task id from pid
        pid_to_local_id,
        SpankItem::JobPidToLocalId,
        pid,
        pid_t,
        u32
    );
    spank_item_getter!(
        /// Local id to global id
        local_to_global_id,
        SpankItem::JobLocalToGlobalId,
        local_id,
        u32,
        u32
    );
    spank_item_getter!(
        /// Global id to local id
        global_to_local_id,
        SpankItem::JobGlobalToLocalId,
        global_id,
        u32,
        u32
    );

    /// Vec of supplementary gids
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
            e => Err(SpankError::from_spank("spank_get_item", e)),
        }
    }

    spank_item_getter!(
        /// Current Slurm version
        slurm_version,
        SpankItem::SlurmVersion,
        &str
    );

    spank_item_getter!(
        /// Slurm version major release
        slurm_version_major,
        SpankItem::SlurmVersionMajor,
        &str
    );
    spank_item_getter!(
        /// Slurm version minor release
        slurm_version_minor,
        SpankItem::SlurmVersionMinor,
        &str
    );
    spank_item_getter!(
        /// Slurm version micro release
        slurm_version_micro,
        SpankItem::SlurmVersionMicro,
        &str
    );
    spank_item_getter!(
        /// CPUs allocated per task/ Returns 1 if --overcommit option is used
        step_cpus_per_task,
        SpankItem::StepCpusPerTask,
        u64
    );

    spank_item_getter!(
        /// Job allocated cores in list format
        job_alloc_cores,
        SpankItem::JobAllocCores,
        &str
    );
    spank_item_getter!(
        ///Job allocated memory in MB
        job_alloc_mem,
        SpankItem::JobAllocMem,
        u64
    );
    spank_item_getter!(
        /// Step allocated cores in list format
        step_alloc_cores,
        SpankItem::StepAllocCores,
        &str
    );
    spank_item_getter!(
        /// Step allocated memory in MB
        step_alloc_mem,
        SpankItem::StepAllocMem,
        u64
    );
    spank_item_getter!(
        /// Job restart count
        slurm_restart_count,
        SpankItem::SlurmRestartCount,
        u32
    );
    spank_item_getter!(
        /// Slurm job array id
        job_array_id,
        SpankItem::JobArrayId,
        u32
    );
    spank_item_getter!(
        /// Slurm job array task id
        job_array_task_id,
        SpankItem::JobArrayTaskId,
        u32
    );
}

fn cstring_escape_null(msg: &str) -> CString {
    // XXX: We can't deal with NULL characters when passing strings to slurm log
    // functions, but how do we expect a plugin author to handle the error if we
    // returned one ? We assume they would prefer that we render them as a 0 in
    // the logs instead.
    let c_safe_msg = msg.split('\u{0000}').collect::<Vec<&str>>().join("0");

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

#[repr(C)]
#[doc(hidden)]
// Simple struct to export a static immutable C string
pub struct StaticCStr(*const u8);
unsafe impl Sync for StaticCStr {}

#[macro_export]
macro_rules! SPANK_PLUGIN {
    ($spank_name:literal, $spank_version:literal, $spank_ty:ty) => {
        #[no_mangle]
        pub static plugin_name: StaticCStr =
            StaticCStr({ concat_bytes!($spank_name, b"\0") } as *const u8);
        #[no_mangle]
        pub static plugin_type: StaticCStr = StaticCStr(b"spank\0" as *const u8);
        #[no_mangle]
        pub static plugin_version: c_int = $spank_version;

        fn _check_spank_trait<T: Plugin>() {}
        fn _t() {
            _check_spank_trait::<$spank_ty>()
        }

        #[derive(Default)]
        struct GlobalData {
            plugin: RefCell<$spank_ty>,
            options: RefCell<OptionCache>,
        }

        lazy_static! {
            // XXX: Slurm should never call us from multiple threads so we could
            // probably just declare unsafe impl Sync for GlobalData and access
            // it directly instead of wrapping it with this Mutex that we lock at
            // the beginning of each callback.
            static ref GLOBAL: Mutex<GlobalData> = Default::default();
        }

        #[no_mangle]
        pub extern "C" fn spank_opt_cb(val: c_int, optarg: *const c_char, _remote: c_int) -> c_int {
            callback_with_globals(|_, cache| {
                let name = cache
                    .options
                    .get(val as usize)
                    .ok_or_else(|| {
                        format!(
                            "Internal spank-rs error: received unexpected option callback {}",
                            val
                        )
                    })
                    .map(|name| name.clone())?;

                let optarg = {
                    if optarg == ptr::null() {
                        None
                    } else {
                        Some(
                            OsStr::from_bytes(unsafe { CStr::from_ptr(optarg) }.to_bytes())
                                .to_os_string(),
                        )
                    }
                };

                cache.values.entry(name).or_insert(vec![]).push(optarg);
                Ok(())
            })
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
                    callback_with_globals(|plugin, options| {
                        let mut spank = SpankHandle {
                            spank: spank,
                            opt_cache: options,
                            argc: ac,
                            argv: argv,
                            spank_opt_cb: spank_opt_cb,
                        };

                        plugin.$rust_spank_cb(&mut spank)
                    })
                }
            };
        }

        fn callback_with_globals<F>(func: F) -> c_int
        where
            F: FnOnce(&mut $spank_ty, &mut OptionCache) -> Result<(), Box<dyn Error>> + UnwindSafe,
        {
            let unwind_res = catch_unwind(|| {
                let global = match GLOBAL.try_lock() {
                    Ok(global) => global,

                    Err(e) => {
                        spank_log(LogLevel::Error, &format!("Internal spank-rs error: {}", e));
                        return bindings::spank_err_ESPANK_ERROR as c_int;
                    }
                };

                let mut plugin = global.plugin.borrow_mut();
                let mut options = global.options.borrow_mut();

                match func(&mut plugin, &mut options) {
                    Ok(()) => bindings::spank_err_ESPANK_SUCCESS as c_int,
                    Err(e) => {
                        plugin.handle_error(e.into());
                        bindings::spank_err_ESPANK_ERROR as c_int
                    }
                }
            });

            match unwind_res {
                Ok(e) => e,
                Err(_) => bindings::spank_err_ESPANK_ERROR as c_int,
            }
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
    fn init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn job_prolog(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn init_post_opt(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn local_user_init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn user_init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn task_init_privileged(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn task_init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn task_post_fork(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn task_exit(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn job_epilog(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn slurmd_exit(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn exit(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn handle_error(&self, error: Box<dyn Error>) {
        spank_log(LogLevel::Error, &error.to_string())
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

#[derive(Debug, Copy, Clone, PartialEq, IntoPrimitive, FromPrimitive)]
#[repr(u32)] // FIXME: force the type generated by bindgen
pub enum SpankApiError {
    #[num_enum(default)]
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

impl Error for SpankApiError {}

impl fmt::Display for SpankApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cerr = unsafe { CStr::from_ptr(bindings::spank_strerror(*self as u32)) };

        if let Ok(err) = cerr.to_str() {
            write!(f, "{}", err)
        } else {
            write!(f, "Unknown Error")
        }
    }
}

impl Error for SpankError {}

#[derive(Debug, Clone)]
pub enum SpankError {
    CStringError(String),
    EnvExists(String),
    IdNotFound(u32),
    PidNotFound(pid_t),
    SpankAPI(String, SpankApiError),
    Utf8Error(String),
}

impl SpankError {
    fn from_os_str(s: &OsStr) -> SpankError {
        SpankError::Utf8Error(s.to_string_lossy().to_string())
    }
    fn from_str(s: &str) -> SpankError {
        SpankError::CStringError(s.to_string())
    }
    fn from_cstr(s: &CStr) -> SpankError {
        SpankError::CStringError(s.to_string_lossy().to_string())
    }
    fn from_spank(name: &str, err: u32) -> SpankError {
        SpankError::SpankAPI(name.to_owned(), SpankApiError::from(err))
    }
}

trait FromNoExist<T> {
    fn from_noexist(v: T) -> SpankError;
}

impl FromNoExist<u32> for SpankError {
    fn from_noexist(v: u32) -> SpankError {
        SpankError::IdNotFound(v)
    }
}

impl FromNoExist<pid_t> for SpankError {
    fn from_noexist(v: pid_t) -> SpankError {
        SpankError::PidNotFound(v)
    }
}

impl fmt::Display for SpankError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpankError::SpankAPI(name, e) => {
                write!(f, "Error calling SPANK API function {}: {}", name, e)
            }
            SpankError::Utf8Error(s) => write!(f, "Cannot parse {} as UTF-8", s),
            SpankError::CStringError(s) => {
                write!(f, "String {} cannot be converted to a C string", s)
            }
            SpankError::EnvExists(s) => write!(
                f,
                "Environment variable {} exists and overwrite was not set",
                s
            ),
            SpankError::PidNotFound(p) => write!(f, "Could not find pid {}", p),
            SpankError::IdNotFound(i) => write!(f, "Could not find id {}", i),
        }
    }
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
    pub fn takes_value(mut self, arg_name: &str) -> Self {
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
    fn init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        println!("Plugin arguments: {:?}", spank.plugin_argv());
        println!("Context is {:?}", spank.context()?);
        self.data = 20;
        spank.register_option(SpankOption::new("opt_a").usage("c'est une option"))?;
        println!("{:?}", spank.job_env());
        Ok(())
    }
    fn task_init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        spank_log(
            LogLevel::Error,
            &format!("{:?}", spank.job_supplmentary_gids()?),
        );
        Ok(())
    }
    fn task_exit(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        spank_log(
            LogLevel::Error,
            &format!("Job env task exit {:?}", spank.job_env()?),
        );

        Ok(())
    }
    fn exit(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        println!("Data is {:?}", self.data);
        println!("CB returned {:?}", spank.get_option_value_os("opt_a"));

        spank_log(LogLevel::Error, "Goodbye from spank logs\n");
        Ok(())
    }
}

SPANK_PLUGIN!(b"toto", 0x130502, TestSpank);
