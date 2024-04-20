//!Rust bindings for writing Slurm SPANK Plugins
//!# Introduction
//!This crate allows to write Slurm SPANK plugins using Rust. To learn more
//!about capabilities available through SPANK please refer to the official
//![`SPANK documentation`].
//!
//! [`SPANK documentation`]: https://slurm.schedmd.com/spank.html
//!
//!To create a SPANK plugin using this crate, you need to define a struct for
//!which you implement the [`Plugin`] trait and to make it available as a SPANK
//!plugin using the [`SPANK_PLUGIN!`] macro.
//!
//!The methods of the Plugin trait correspond to the callbacks defined by the
//!SPANK API such as [`init_post_opt`], [`task_post_fork`] etc. These methods
//!have a default implementation which means you only need to implement the
//!callbacks relevant for your plugin.
//!
//! [`init_post_opt`]: crate::Plugin::init_post_opt
//!
//! [`task_post_fork`]: crate::Plugin::task_post_fork
//!
//!Each callback method is passed a [`SpankHandle`] reference which allows to
//!interact with Slurm through the SPANK API.
//!
//!When returning an [`Err`] from a callback an error message will be displayed
//!and/or logged by default, depending on the context. This behaviour may be
//!overridden by the [`report_error`] method. A default [`Subscriber`] is also
//!configured to facilitate the use of the [`tracing`] crate for logging and
//!error reporting while using SPANK log facilities, such as in the example
//!below. This can be overridden by the [`setup`] method.
//!
//! [`report_error`]: crate::Plugin::report_error
//!
//! [`Subscriber`]: tracing::Subscriber
//!
//! [`setup`]: crate::Plugin::setup
//!
//!# Example: hello.so
//!The following example implements a simple hello world plugin. A more complete
//!example is provided in the example directory of the repository which shows
//!how to implement the same renice plugin that is given as an example of the C
//!SPANK API in the Slurm [`SPANK documentation`].
//!```rust,no_run
#![doc = include_str!("hello.md")]
//! ```
//! The following Cargo.toml can be used to build this example plugin
//!```toml
//! [package]
//! name = "slurm-spank-hello"
//! version = "0.1.0"
//! edition = "2021"
//!
//! [lib]
//! crate-type = ["cdylib"]
//!
//! [dependencies]
//! eyre = "0.6.8"
//! slurm-spank = "0.3"
//! tracing = "0.1.37"
//!```
use lazy_static::lazy_static;
use libc::{gid_t, pid_t, uid_t};
use num_enum::{FromPrimitive, IntoPrimitive, TryFromPrimitive};
use std::borrow::Cow;
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
use tracing::{error, span};
use tracing_core::{Event, Subscriber};
use tracing_subscriber::fmt::{
    format::Writer, layer, FmtContext, FormatEvent, FormatFields, FormattedFields,
};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{EnvFilter, Registry};

#[doc(hidden)]
pub mod spank_sys;
#[doc(hidden)]
pub use byte_strings;

/// Handle to the Slurm interface exposed to SPANK plugins. It provides methods
/// to query Slurm from a plugin.
pub struct SpankHandle<'a> {
    spank: spank_sys::spank_t,
    argc: c_int,
    argv: *const *const c_char,
    opt_cache: &'a mut OptionCache,
}

macro_rules! spank_item_getter {
    ($(#[$outer:meta])* $name:ident, $spank_item:path, $arg_name:ident, $arg_type:ty, $result_type:ty) => {
        $(#[$outer])*
        pub fn $name(&self, $arg_name: $arg_type) -> Result<$result_type, SpankError> {
            let mut res: $result_type = <$result_type>::default();
            let res_ptr: *mut $result_type = &mut res;
            match unsafe {
                spank_sys::spank_get_item(self.spank, $spank_item.into(), $arg_name, res_ptr)
            } {
                spank_sys::ESPANK_SUCCESS => Ok(res),
                spank_sys::slurm_err_t_ESPANK_NOEXIST => Err(SpankError::from_noexist($arg_name)),
                e => Err(SpankError::from_spank_item("spank_get_item", $spank_item, e)),
            }
        }
    };
    ($(#[$outer:meta])* $name:ident, $spank_item:path, &str) => {
        $(#[$outer])*
        pub fn $name(&self) -> Result<&str, SpankError> {
            let mut res: *const c_char = ptr::null_mut();
            let res_ptr: *mut *const c_char = &mut res;
            match unsafe { spank_sys::spank_get_item(self.spank, $spank_item.into(), res_ptr) } {
                spank_sys::ESPANK_SUCCESS => {
                    if res.is_null() {
                        panic!("Received unexpected null pointer from spank_get_item")
                    } else {
                        let cstr = unsafe { CStr::from_ptr(res) };
                        cstr.to_str()
                            .map_err(|_| SpankError::Utf8Error(cstr.to_string_lossy().to_string()))
                    }
                }
                e => Err(SpankError::from_spank_item("spank_get_item", $spank_item, e)),
            }
        }
    };
    ($(#[$outer:meta])* $name:ident, $spank_item:path,$result_type:ty) => {
        $(#[$outer])*
        pub fn $name(&self) -> Result<$result_type, SpankError> {
            let mut res: $result_type = <$result_type>::default();
            let res_ptr: *mut $result_type = &mut res;
            match unsafe { spank_sys::spank_get_item(self.spank, $spank_item.into(), res_ptr) } {
                spank_sys::ESPANK_SUCCESS => Ok(res),
                e => Err(SpankError::from_spank_item("spank_get_item", $spank_item, e)),
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
            value
                .to_str()
                .ok_or_else(|| SpankError::from_os_str(value))?,
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
#[derive(Default, Debug)]
#[doc(hidden)]
pub struct OptionCache {
    pub options: Vec<String>,
    pub values: HashMap<String, Option<OsString>>,
}

impl<'a> SpankHandle<'a> {
    /// Returns the context in which the calling plugin is loaded.
    pub fn context(&self) -> Result<Context, SpankError> {
        let ctx = unsafe { spank_sys::spank_context() };
        Context::try_from(ctx).map_err(|_| {
            SpankError::from_spank("spank_context", spank_sys::slurm_err_t_ESPANK_ERROR)
        })
    }

    /// Registers a plugin-provided option dynamically. This function is only
    /// valid when called from a plugin's `init()`, and must be guaranteed to be
    /// called in all contexts in which it is used (local, remote, allocator).
    pub fn register_option(&mut self, spank_opt: SpankOption) -> Result<(), SpankError> {
        let arginfo = match &spank_opt.arginfo {
            None => None,
            Some(info) => Some(CString::new(info as &str).map_err(|_| SpankError::from_str(info))?),
        };
        let name = CString::new(&spank_opt.name as &str)
            .map_err(|_| SpankError::from_str(&spank_opt.name))?;
        let usage = match spank_opt.usage {
            None => None,
            Some(usage) => {
                Some(CString::new(&usage as &str).map_err(|_| SpankError::from_str(&usage))?)
            }
        };

        let mut c_spank_opt = spank_sys::spank_option {
            name: name.as_ptr(),
            has_arg: arginfo.is_some() as i32,
            cb: Some(spank_option_callback),
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

        match unsafe { spank_sys::spank_option_register(self.spank, &mut c_spank_opt) } {
            spank_sys::ESPANK_SUCCESS => {
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

    /// Prepends the vector of str `argv` to the argument vector of the task
    /// to be spawned. This function can be invoked from the following
    /// functions: slurm_spank_task_init_privileged, and slurm_spank_task_init.
    ///
    /// An error is returned if called outside of a task context or if the
    /// argument vector is invalid.
    pub fn prepend_task_argv(&self, argv: Vec<&str>) -> Result<(), SpankError> {
        let c_argv: Vec<CString> = argv
            .iter()
            .map(|&arg| CString::new(arg).map_err(|_| SpankError::from_str(arg)))
            .collect::<Result<Vec<CString>, SpankError>>()?;

        self.prepend_task_cstring(c_argv)
    }

    /// Prepends the vector of OsStr `argv` to the argument vector of the task
    /// to be spawned. This function can be invoked from the following
    /// functions: slurm_spank_task_init_privileged, and slurm_spank_task_init.
    ///
    /// An error is returned if called outside of a task context or if the
    /// argument vector is invalid.
    pub fn prepend_task_argv_os(&self, argv: Vec<&OsStr>) -> Result<(), SpankError> {
        let c_argv: Vec<CString> = argv
            .iter()
            .map(|&arg| {
                CString::new(arg.as_bytes())
                    .map_err(|_| SpankError::CStringError(arg.to_string_lossy().to_string()))
            })
            .collect::<Result<Vec<CString>, SpankError>>()?;

        self.prepend_task_cstring(c_argv)
    }

    fn prepend_task_cstring(&self, argv: Vec<CString>) -> Result<(), SpankError> {
        let mut c_argv_ptrs: Vec<*const c_char> = argv.iter().map(|arg| arg.as_ptr()).collect();
        let c_argv_ptr: *mut *const c_char = c_argv_ptrs.as_mut_ptr();
        let count = i32::try_from(argv.len()).map_err(|_| SpankError::Overflow(argv.len()))?;

        match unsafe { spank_sys::spank_prepend_task_argv(self.spank, count, c_argv_ptr) } {
            spank_sys::ESPANK_SUCCESS => Ok(()),
            e => Err(SpankError::from_spank("spank_prepend_task_argv", e)),
        }
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
    pub fn getenv<N: AsRef<OsStr>>(&self, name: N) -> Result<Option<String>, SpankError> {
        match self.do_getenv_os(name, spank_sys::spank_getenv)? {
            None => Ok(None),
            Some(env) => Ok(Some(
                env.into_string().map_err(|e| SpankError::from_os_str(&e))?,
            )),
        }
    }

    ///  Retrieves the environment variable `name` from the job's environment as
    ///  a lossy String
    ///
    ///  If the value contains invalid UTF-8 code points, those invalid points
    ///  will be replaced with � (U+FFFD). This function returns Ok(none) if the
    ///  environment variable is not set. It returns an error if called outside
    ///  of remote context. To access job environment variables from local
    ///  context, use std::env directly
    pub fn getenv_lossy<N: AsRef<OsStr>>(&self, name: N) -> Result<Option<String>, SpankError> {
        self.do_getenv_os(name, spank_sys::spank_getenv)
            .map(|env| env.map(|s| s.to_string_lossy().into_owned()))
    }

    ///  Retrieves the environment variable `name` from the job's environment as
    ///  an OsString
    ///
    ///  The return value is an OsString which can hold arbitrary sequences of
    ///  bytes on Unix-like systems. This function returns Ok(none) if the
    ///  environment variable is not set. It returns an error if called outside
    ///  of remote context. To access job environment variables from local
    ///  context, use std::env directly
    pub fn getenv_os<N: AsRef<OsStr>>(&self, name: N) -> Result<Option<OsString>, SpankError> {
        self.do_getenv_os(name, spank_sys::spank_getenv)
    }

    ///  Retrieves the environment variable `name` from the job's control
    ///  environment as a String
    ///
    ///  This function returns Ok(none) if the environment variable is not set.
    ///  It returns an error if the value is not a valid UTF-8 string or if
    ///  called outside of local/allocator context. To access job control environment
    ///  variables from job script context, use std::env directly.
    pub fn job_control_getenv<N: AsRef<OsStr>>(
        &self,
        name: N,
    ) -> Result<Option<String>, SpankError> {
        match self.do_getenv_os(name, spank_sys::spank_job_control_getenv)? {
            None => Ok(None),
            Some(env) => Ok(Some(
                env.into_string().map_err(|e| SpankError::from_os_str(&e))?,
            )),
        }
    }

    ///  Retrieves the environment variable `name` from the job's control
    ///  environment as a lossy String
    ///
    ///  If the value contains invalid UTF-8 code points, those invalid points
    ///  will be replaced with � (U+FFFD). This function returns Ok(none) if the
    ///  environment variable is not set. It returns an error if called outside
    ///  of local/allocator context. To access job control environment variables from
    ///  job script context, use std::env directly.
    pub fn job_control_getenv_lossy<N: AsRef<OsStr>>(
        &self,
        name: N,
    ) -> Result<Option<String>, SpankError> {
        self.do_getenv_os(name, spank_sys::spank_job_control_getenv)
            .map(|env| env.map(|s| s.to_string_lossy().into_owned()))
    }

    ///  Retrieves the environment variable `name` from the job's control
    ///  environment as an OsString
    ///
    ///  The return value is an OsString which can hold arbitrary sequences of
    ///  bytes on Unix-like systems. This function returns Ok(none) if the
    ///  environment variable is not set. It returns an error if called outside
    ///  of local/allocator context. To access job control environment variables from
    ///  job script context, use std::env directly.
    pub fn job_control_getenv_os<N: AsRef<OsStr>>(
        &self,
        name: N,
    ) -> Result<Option<OsString>, SpankError> {
        self.do_getenv_os(name, spank_sys::spank_job_control_getenv)
    }

    fn do_getenv_os<N: AsRef<OsStr>>(
        &self,
        name: N,
        spank_fn: unsafe extern "C" fn(
            spank_sys::spank_t,
            *const c_char,
            *mut c_char,
            c_int,
        ) -> spank_sys::spank_err_t,
    ) -> Result<Option<OsString>, SpankError> {
        let mut max_size = 4096;
        let c_name = CString::new(name.as_ref().as_bytes())
            .map_err(|_| SpankError::from_str(&name.as_ref().to_string_lossy()))?;
        loop {
            let mut buffer = vec![0; max_size];
            let buffer_ptr = buffer.as_mut_ptr();
            match unsafe {
                spank_fn(
                    self.spank,
                    c_name.as_ptr(),
                    buffer_ptr as *mut c_char,
                    max_size as i32,
                )
            } {
                spank_sys::slurm_err_t_ESPANK_ENV_NOEXIST => return Ok(None),
                spank_sys::ESPANK_SUCCESS => {
                    let cstr = unsafe { CStr::from_ptr(buffer_ptr) };
                    return Ok(Some(OsStr::from_bytes(cstr.to_bytes()).to_os_string()));
                }
                spank_sys::slurm_err_t_ESPANK_NOSPACE => {
                    max_size *= 2;
                    continue;
                }
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
        self.do_setenv(name, value, overwrite, spank_sys::spank_setenv)
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
        self.do_setenv(name, value, overwrite, spank_sys::spank_job_control_setenv)
    }

    pub fn do_setenv<N: AsRef<OsStr>, V: AsRef<OsStr>>(
        &self,
        name: N,
        value: V,
        overwrite: bool,
        spank_fn: unsafe extern "C" fn(
            spank_sys::spank_t,
            *const c_char,
            *const c_char,
            c_int,
        ) -> spank_sys::spank_err_t,
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
            spank_sys::ESPANK_SUCCESS => Ok(()),
            spank_sys::slurm_err_t_ESPANK_ENV_EXISTS => Err(SpankError::EnvExists(
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
        self.do_unsetenv(name, spank_sys::spank_unsetenv)
    }

    /// Unsets the environment variable `name` in the job's control environment.
    ///
    /// This function is a no-op if the variable is already unset. It will
    /// return an error if called outside of local/allocator context. To access job
    /// control environment variables from remote context, use std::env
    /// directly.
    pub fn job_control_unsetenv<N: AsRef<OsStr>>(&self, name: N) -> Result<(), SpankError> {
        self.do_unsetenv(name, spank_sys::spank_job_control_unsetenv)
    }

    fn do_unsetenv<N: AsRef<OsStr>>(
        &self,
        name: N,
        spank_fn: unsafe extern "C" fn(spank_sys::spank_t, *const c_char) -> spank_sys::spank_err_t,
    ) -> Result<(), SpankError> {
        let c_name = CString::new(name.as_ref().as_bytes())
            .map_err(|_| SpankError::from_os_str(name.as_ref()))?;

        match unsafe { spank_fn(self.spank, c_name.as_ptr()) } {
            spank_sys::ESPANK_SUCCESS => Ok(()),
            e => Err(SpankError::from_spank("spank_unsetenv", e)),
        }
    }

    fn getopt_os(&self, name: &str) -> Result<Option<OsString>, SpankError> {
        let name_c = if let Ok(n) = CString::new(name) {
            n
        } else {
            return Err(SpankError::from_str(name));
        };

        let mut c_spank_opt = spank_sys::spank_option {
            name: name_c.as_ptr(),
            has_arg: 1,
            cb: None,
            usage: ptr::null(),
            arginfo: ptr::null(),
            val: 0,
        };

        let mut optarg: *mut c_char = ptr::null_mut();

        match unsafe { spank_sys::spank_option_getopt(self.spank, &mut c_spank_opt, &mut optarg) } {
            spank_sys::ESPANK_SUCCESS => {
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
    /// Returns the value set for the option `name` as a lossy String
    ///
    /// If the value contains invalid UTF-8 code points, those invalid points
    /// will be replaced with � (U+FFFD). If the option was specified multiple
    /// times, this function returns the last value provided.
    ///
    /// *WARNING*: If options have not yet been processed (e.g in init callbacks
    /// or all slurmd contexts), this function will always return None.
    ///
    /// *WARNING*: This function always returns None for options which don't
    /// take values (flag options created without takes_value()) no matter whether
    /// they were used or not. To check whether a flag was set, use
    /// is_option_set.
    pub fn get_option_value_lossy(&self, name: &str) -> Option<Cow<'_, str>> {
        self.get_option_value_os(name).map(os_value_to_lossy)
    }

    /// Returns the value set for the option `name` as a String
    ///
    /// An error is returned if the value cannot be converted to a String. If
    /// the option was specified multiple times, it returns the last value
    /// provided.
    ///
    /// *WARNING*: If options have not yet been processed (e.g in init callbacks
    /// or all slurmd contexts), this function will always return None.
    ///
    /// *WARNING*: This function always returns None for options which don't
    /// take values (flag options created without takes_value()) no matter whether
    /// they were used or not. To check whether a flag was set, use
    /// is_option_set.
    pub fn get_option_value(&self, name: &str) -> Result<Option<Cow<'_, str>>, SpankError> {
        match self.get_option_value_os(name) {
            Some(val) => Ok(Some(os_value_to_str(val)?)),
            None => Ok(None),
        }
    }

    /// Returns the value set for the option `name` as an OsString
    ///
    /// If the option was specified multiple times, it returns the last value
    /// provided.
    ///
    /// *WARNING*: If options have not yet been processed (e.g in init callbacks
    /// or all slurmd contexts), this function will always return None.
    ///
    /// *WARNING*: This function always returns None for options which don't
    /// take values (flag options created without takes_value()) no matter whether
    /// they were used or not. To check whether a flag was set, use
    /// get_option_count.
    pub fn get_option_value_os(&self, name: &str) -> Option<Cow<'_, OsStr>> {
        match self.context() {
            Ok(Context::JobScript) => self
                .getopt_os(name)
                .ok() // We made sure call from the correct context
                .map(|opt| opt.map(Cow::from))
                .unwrap_or(None),
            _ => {
                if let Some(Some(ref value)) = self.opt_cache.values.get(name) {
                    Some(Cow::from(value))
                } else {
                    None
                }
            }
        }
    }

    /// Returns whether an option was set
    ///
    /// Use this function to process flag options.
    ///
    /// *WARNING*: If options have not yet been processed (e.g in init callbacks
    /// or all slurmd contexts), this function will always return false.
    pub fn is_option_set(&self, name: &str) -> bool {
        match self.context() {
            Ok(Context::JobScript) => self.getopt_os(name).is_ok(),
            _ => self.opt_cache.values.get(name).is_some(),
        }
    }

    spank_item_getter!(
        /// Returns the primary group id
        job_gid,
        SpankItem::JobGid,
        gid_t
    );
    spank_item_getter!(
        /// Returns the user id
        job_uid,
        SpankItem::JobUid,
        uid_t
    );
    spank_item_getter!(
        /// Returns the  job id
        job_id,
        SpankItem::JobId,
        u32
    );
    spank_item_getter!(
        /// Returns the job step id
        job_stepid,
        SpankItem::JobStepid,
        u32
    );
    spank_item_getter!(
        /// Returns the total number of nodes in job
        job_nnodes,
        SpankItem::JobNnodes,
        u32
    );
    spank_item_getter!(
        /// Returns the relative id of this node
        job_nodeid,
        SpankItem::JobNodeid,
        u32
    );
    spank_item_getter!(
        /// Returns the number of local tasks
        job_local_task_count,
        SpankItem::JobLocalTaskCount,
        u32
    );
    spank_item_getter!(
        /// Returns the total number of tasks in job
        job_total_task_count,
        SpankItem::JobTotalTaskCount,
        u32
    );
    spank_item_getter!(
        /// Returns the number of CPUs used by this job
        job_ncpus,
        SpankItem::JobNcpus,
        u16
    );

    /// Returns the job command arguments as Vec<&str>. An error is returned if
    /// arguments are not valid UTF-8
    pub fn job_argv(&self) -> Result<Vec<&str>, SpankError> {
        self.job_argv_c()
            .and_then(|(argc, argv)| self.argv_to_vec(argc, argv))
    }

    /// Returns the job command args as Vec<&OsStr>
    pub fn job_argv_os(&self) -> Result<Vec<&OsStr>, SpankError> {
        self.job_argv_c()
            .map(|(argc, argv)| self.argv_to_vec_os(argc, argv))
    }

    fn job_argv_c(&self) -> Result<(usize, *const *const c_char), SpankError> {
        let mut argc: c_int = 0;
        let mut argv: *const *const c_char = ptr::null_mut();

        let argc_ptr: *mut c_int = &mut argc;
        let argv_ptr: *mut *const *const c_char = &mut argv;

        match unsafe {
            spank_sys::spank_get_item(self.spank, SpankItem::JobArgv.into(), argc_ptr, argv_ptr)
        } {
            spank_sys::ESPANK_SUCCESS => {
                if argv.is_null() {
                    panic!("spank_get_item returned unexpected NULL ptr");
                }
                Ok((argc as usize, argv))
            }
            e => Err(SpankError::from_spank("spank_get_item", e)),
        }
    }

    /// Returns the job environment variables as a Vec<&str>. An error is
    /// returned if variables are not valid UTF-8
    pub fn job_env(&self) -> Result<Vec<&str>, SpankError> {
        self.job_env_c()
            .and_then(|(argc, argv)| self.argv_to_vec(argc, argv))
    }

    /// Returns the job environment variables as an array of Vec<&OsStr>
    pub fn job_env_os(&self) -> Result<Vec<&OsStr>, SpankError> {
        self.job_env_c()
            .map(|(argc, argv)| self.argv_to_vec_os(argc, argv))
    }

    fn job_env_c(&self) -> Result<(usize, *const *const c_char), SpankError> {
        let mut envv: *const *const c_char = ptr::null_mut();

        match unsafe { spank_sys::spank_get_item(self.spank, SpankItem::JobEnv.into(), &mut envv) }
        {
            spank_sys::ESPANK_SUCCESS => {
                if envv.is_null() {
                    panic!("spank_get_item returned unexpected NULL ptr")
                }
                let mut argc: isize = 0;
                while !unsafe { *envv.offset(argc) }.is_null() {
                    argc += 1;
                }
                Ok((argc as usize, envv))
            }
            e => Err(SpankError::from_spank("spank_get_item", e)),
        }
    }

    spank_item_getter!(
        /// Returns the local task id
        task_id,
        SpankItem::TaskId,
        c_int
    );

    spank_item_getter!(
        /// Returns the global task id
        task_global_id,
        SpankItem::TaskGlobalId,
        u32
    );

    spank_item_getter!(
        /// Returns the exit status of the current task if exited
        task_exit_status,
        SpankItem::TaskExitStatus,
        c_int
    );

    spank_item_getter!(
        /// Returns the pid of the current task
        task_pid,
        SpankItem::TaskPid,
        pid_t
    );
    spank_item_getter!(
        /// Returns the the global task id corresponding to the specified pid
        pid_to_global_id,
        SpankItem::JobPidToGlobalId,
        pid,
        pid_t,
        u32
    );
    spank_item_getter!(
        /// Returns the local task id corresponding to the specified pid
        pid_to_local_id,
        SpankItem::JobPidToLocalId,
        pid,
        pid_t,
        u32
    );
    spank_item_getter!(
        /// Returns the local task id corresponding to the specified global id
        local_to_global_id,
        SpankItem::JobLocalToGlobalId,
        local_id,
        u32,
        u32
    );
    spank_item_getter!(
        /// Returns the global task id corresponding to the specified local id
        global_to_local_id,
        SpankItem::JobGlobalToLocalId,
        global_id,
        u32,
        u32
    );

    /// Returns the list of supplementary gids for the current job
    pub fn job_supplementary_gids(&self) -> Result<Vec<gid_t>, SpankError> {
        let mut gidc: c_int = 0;
        let mut gidv: *const gid_t = ptr::null_mut();

        let gidc_ptr: *mut c_int = &mut gidc;
        let gidv_ptr: *mut *const gid_t = &mut gidv;

        match unsafe {
            spank_sys::spank_get_item(
                self.spank,
                SpankItem::JobSupplementaryGids.into(),
                gidv_ptr,
                gidc_ptr,
            )
        } {
            spank_sys::ESPANK_SUCCESS => {
                Ok(unsafe { slice::from_raw_parts(gidv, gidc as usize) }.to_vec())
            }
            e => Err(SpankError::from_spank("spank_get_item", e)),
        }
    }

    spank_item_getter!(
        /// Returns the current Slurm version
        slurm_version,
        SpankItem::SlurmVersion,
        &str
    );

    spank_item_getter!(
        /// Returns the major release number of Slurm
        slurm_version_major,
        SpankItem::SlurmVersionMajor,
        &str
    );
    spank_item_getter!(
        /// Returns the minor release number of Slurm
        slurm_version_minor,
        SpankItem::SlurmVersionMinor,
        &str
    );
    spank_item_getter!(
        /// Returns the micro release number of Slurm
        slurm_version_micro,
        SpankItem::SlurmVersionMicro,
        &str
    );
    spank_item_getter!(
        /// Returns the number of CPUs allocated per task. Returns 1 if --overcommit option is used
        step_cpus_per_task,
        SpankItem::StepCpusPerTask,
        u64
    );

    spank_item_getter!(
        /// Returns the list of allocated cores for the job
        job_alloc_cores,
        SpankItem::JobAllocCores,
        &str
    );
    spank_item_getter!(
        /// Returns the amount of allocated memory for the job in MB
        job_alloc_mem,
        SpankItem::JobAllocMem,
        u64
    );
    spank_item_getter!(
        /// Returns the list of allocated cores for the step
        step_alloc_cores,
        SpankItem::StepAllocCores,
        &str
    );
    spank_item_getter!(
        /// Returns the amount of allocated memory for the step in MB
        step_alloc_mem,
        SpankItem::StepAllocMem,
        u64
    );
    spank_item_getter!(
        /// Returns the restart count for the job
        slurm_restart_count,
        SpankItem::SlurmRestartCount,
        u32
    );
    spank_item_getter!(
        /// Returns the job array id
        job_array_id,
        SpankItem::JobArrayId,
        u32
    );
    spank_item_getter!(
        /// Returns the job array task id
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

/// Log level for SPANK logging functions
pub enum LogLevel {
    Error,
    Info,
    Verbose,
    Debug,
    Debug2,
    Debug3,
}

static FORMAT_STRING: [u8; 3] = *b"%s\0";

/// Log messages through SPANK
pub fn spank_log(level: LogLevel, msg: &str) {
    let c_msg = cstring_escape_null(msg);
    let c_format_string = FORMAT_STRING.as_ptr() as *const c_char;

    match level {
        LogLevel::Error => unsafe { spank_sys::slurm_error(c_format_string, c_msg.as_ptr()) },
        LogLevel::Info => unsafe { spank_sys::slurm_info(c_format_string, c_msg.as_ptr()) },
        LogLevel::Verbose => unsafe { spank_sys::slurm_verbose(c_format_string, c_msg.as_ptr()) },
        LogLevel::Debug => unsafe { spank_sys::slurm_debug(c_format_string, c_msg.as_ptr()) },
        LogLevel::Debug2 => unsafe { spank_sys::slurm_debug2(c_format_string, c_msg.as_ptr()) },
        LogLevel::Debug3 => unsafe { spank_sys::slurm_debug3(c_format_string, c_msg.as_ptr()) },
    }
}

pub fn slurm_spank_log(msg: &str) {
    let c_msg = cstring_escape_null(msg);
    let c_format_string = FORMAT_STRING.as_ptr() as *const c_char;
    unsafe { spank_sys::slurm_spank_log(c_format_string, c_msg.as_ptr()) }
}

#[macro_export]
/// Log messages through SPANK at the error level
macro_rules! spank_log_error {
    ($($arg:tt)*) => ({
        $crate::spank_log($crate::LogLevel::Error,&format!($($arg)*));
    })
}

#[macro_export]
/// Log messages through SPANK at the info level
macro_rules! spank_log_info {
    ($($arg:tt)*) => ({
        $crate::spank_log($crate::LogLevel::Info, &format!($($arg)*));
    })
}

#[macro_export]
/// Log messages through SPANK at the verbose level
macro_rules! spank_log_verbose {
    ($($arg:tt)*) => ({
        $crate::spank_log($crate::LogLevel::Verbose, &format!($($arg)*));
    })
}

#[macro_export]
/// Log messages through SPANK at the debug level
macro_rules! spank_log_debug {
    ($($arg:tt)*) => ({
        $crate::spank_log($crate::LogLevel::Debug, &format!($($arg)*));
    })
}

#[macro_export]
/// Log messages through SPANK at the debug2 level
macro_rules! spank_log_debug2 {
    ($($arg:tt)*) => ({
        $crate::spank_log($crate::LogLevel::Debug2, &format!($($arg)*));
    })
}

#[macro_export]
/// Log messages through SPANK at the debug3 level
macro_rules! spank_log_debug3 {
    ($($arg:tt)*) => ({
        $crate::spank_log($crate::LogLevel::Debug3, &format!($($arg)*));
    })
}

#[macro_export]
/// Log messages back to the user at the error level without prepending "error:"
macro_rules! spank_log_user {
    ($($arg:tt)*) => ({
        $crate::slurm_spank_log(&format!($($arg)*));
    })
}

// XXX: Slurm should only call us in a sequential and non-reentrant way but Rust
// doesn't know that. The overhead of locking these Mutex at each Slurm callback
// should be negligible and we'll get a clear error if something is called out
// of order by mistake. However this is not ideal because it requires the Plugin
// to be Send which can be restricting. We should probably confirm with Slurm
// devs that all calls are sequential and switch to a static mut or similar.
lazy_static! {
    static ref OPTION_CACHE: Mutex<OptionCache> = Mutex::new(OptionCache::default());
    static ref PLUGIN: Mutex<Option<Box<dyn Plugin>>> = Mutex::new(None);
}

#[doc(hidden)]
pub fn spank_callback_with_globals<P: Plugin + Default + 'static, F>(func: F) -> c_int
where
    F: FnOnce(&mut dyn Plugin, &mut OptionCache, bool) -> Result<(), Box<dyn Error>> + UnwindSafe,
{
    let unwind_res = catch_unwind(|| {
        // These Mutexes should never be contended unless something unreoverable
        // happened before
        let mut opt_cache = OPTION_CACHE
            .try_lock()
            .expect("Failed to acquire global options mutex");
        let mut plugin_option = PLUGIN
            .try_lock()
            .expect("Failed to acquire global plugin mutex");

        let mut need_setup = false;

        let mut plugin = plugin_option.take().unwrap_or_else(|| {
            let p = P::default();
            need_setup = true;
            Box::new(p)
        });

        let err = match func(plugin.as_mut(), &mut opt_cache, need_setup) {
            Ok(()) => 0,
            Err(_) => -1,
        };
        plugin_option.replace(plugin);

        err
    });

    match unwind_res {
        Ok(e) => e,
        Err(panic) => {
            let panic_string = panic
                .downcast::<&str>()
                .map(|b| b.to_string())
                .or_else(|panic| panic.downcast::<String>().map(|s| *s))
                .unwrap_or_else(|_| "non-string panic".to_string());

            spank_log_error!(
                "Caught panic while running spank callback: {}",
                panic_string
            );
            -1
        }
    }
}

#[no_mangle]
// We pass this callback to process all spank options
// It just stores which options were set in a cache for later retrieval
extern "C" fn spank_option_callback(
    val: std::os::raw::c_int,
    optarg: *const std::os::raw::c_char,
    _remote: std::os::raw::c_int,
) -> std::os::raw::c_int {
    // This Mutex should never be contended unless something unrecoverable
    // already happened before
    let mut opt_cache = OPTION_CACHE
        .try_lock()
        .expect("Failed to acquire global options mutex");

    let name = opt_cache.options.get(val as usize).cloned();

    let name = match name {
        None => {
            spank_log(
                LogLevel::Error,
                &format!(
                    "Internal spank-rs error: received unexpected option callback {}",
                    val
                ),
            );
            return -1;
        }
        Some(name) => name,
    };

    let optarg = {
        if optarg.is_null() {
            None
        } else {
            Some(
                std::ffi::OsStr::from_bytes(unsafe { std::ffi::CStr::from_ptr(optarg) }.to_bytes())
                    .to_os_string(),
            )
        }
    };

    opt_cache.values.insert(name, optarg);
    0
}

#[doc(hidden)]
// This function only public so that it may be called from the callbacks
// generated by the macro. It should not be called to create handles manually.
pub fn init_spank_handle(
    spank: spank_sys::spank_t,
    argc: c_int,
    argv: *const *const c_char,
    opt_cache: &mut OptionCache,
) -> SpankHandle {
    SpankHandle {
        spank,
        argc,
        argv,
        opt_cache,
    }
}

#[doc(hidden)]
// This function is only public so that it may be called from the callbacks
// generated by the macro.
pub fn make_cb_span(id: &str, cb: &str, ctx: &str, task_id: Option<u32>) -> tracing::Span {
    if let Some(task_id) = task_id {
        span!(tracing::Level::DEBUG, "spank", id, cb, ctx, task_id)
    } else {
        span!(tracing::Level::DEBUG, "spank", id, cb, ctx)
    }
}

pub use spank_sys::SLURM_VERSION_NUMBER;

#[macro_export]
/// Export a Plugin to make it available to the Slurm plugin loader
///
/// # Example
///
///```rust,no_run
///SPANK_PLUGIN!(b"renice", SLURM_VERSION_NUMBER, SpankRenice);
///```
///
/// The first argument is the name of the SPANK plugin. It has to be provided as a byte string.
///
/// The second argument is the Slurm version for which the plugin is built, specified in hexadecimal (2 digits per version component).
/// The SLURM_VERSION_NUMBER constant can be used. It refers to the version of the Slurm headers that the plugin is built against.
///
/// The last argument is a struct for which the Plugin trait has been implemented
macro_rules! SPANK_PLUGIN {
    ($spank_name:literal, $spank_version:expr, $spank_ty:ty) => {
        const fn byte_string_size<T>(_: &T) -> usize {
            std::mem::size_of::<T>()
        }
        #[no_mangle]
        pub static plugin_name: [u8; byte_string_size($spank_name) + 1] =
            *$crate::byte_strings::concat_bytes!($spank_name, "\0");
        #[no_mangle]
        pub static mut plugin_type: [u8; 6] = *b"spank\0";
        #[no_mangle]
        pub static plugin_version: std::os::raw::c_uint = $spank_version;

        fn _check_spank_trait<T: Plugin>() {}
        fn _t() {
            _check_spank_trait::<$spank_ty>()
        }

        macro_rules! spank_hook {
            ($c_spank_cb:ident, $rust_spank_cb:ident) => {
                #[no_mangle]
                #[doc(hidden)]
                pub extern "C" fn $c_spank_cb(
                    spank: $crate::spank_sys::spank_t,
                    ac: std::os::raw::c_int,
                    argv: *const *const std::os::raw::c_char,
                ) -> std::os::raw::c_int {
                    $crate::spank_callback_with_globals::<$spank_ty, _>(
                        |plugin, options, need_setup| {
                            let mut spank = $crate::init_spank_handle(spank, ac, argv, options);

                            if need_setup {
                                plugin.setup(&mut spank).map_err(|e| {
                                    plugin.report_error(e.as_ref());
                                    e
                                })?;
                            }

                            let context = spank
                                .context()
                                .map(|ctx| format!("{:?}", ctx))
                                .unwrap_or("Error".to_string());

                            let tid = spank.task_global_id().ok();

                            let span = $crate::make_cb_span(
                                std::ffi::CStr::from_bytes_with_nul(&plugin_name)?.to_str()?,
                                stringify!($c_spank_cb),
                                &context,
                                tid,
                            );
                            let _guard = span.enter();

                            unsafe {
                                plugin.$rust_spank_cb(&mut spank).map_err(|e| {
                                    plugin.report_error(e.as_ref());
                                    e
                                })
                            }
                        },
                    )
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

/// Implement this trait to create a SPANK plugin
/// # Safety
/// The task callbacks (task_init, task_init_privileged, ...) are called from child processes which slurmstepd creates by forking itself.
/// This may lead to deadlocks or other issues if the Rust plugin is multi-threaded (see <https://man7.org/linux/man-pages/man7/signal-safety.7.html>)
#[allow(unused_variables)]
pub unsafe trait Plugin: Send {
    /// Called just after plugins are loaded.
    ///
    /// In remote context, this is just after job step is initialized. This
    /// function is called before any plugin option processing.
    fn init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    /// Called at the same time as the job prolog.
    ///
    /// If this function returns an error and the SPANK plugin that contains it
    /// is required in the plugstack.conf, the node that this is run on will be
    /// drained.
    fn job_prolog(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    /// Called at the same point as slurm_spank_init, but after all user options
    /// to the plugin have been processed.
    ///
    /// The reason that the init and init_post_opt callbacks are separated is so
    /// that plugins can process system-wide options specified in plugstack.conf
    /// in the init callback, then process user options, and finally take some
    /// action in slurm_spank_init_post_opt if necessary. In the case of a
    /// heterogeneous job, slurm_spank_init is invoked once per job component.
    fn init_post_opt(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    /// Called in local (srun) context only after all options have been
    /// processed.
    ///
    /// This is called after the job ID and step IDs are available. This happens
    /// in srun after the allocation is made, but before tasks are launched.
    fn local_user_init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    /// Called after privileges are temporarily dropped. (remote context only)
    fn user_init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    /// Called for each task just after fork, but before all elevated privileges
    /// are dropped. (remote context only)
    fn task_init_privileged(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    /// Called for each task just before execve (2).
    ///
    /// If you are restricting memory with cgroups, memory allocated here will be
    /// in the job's cgroup. (remote context only)
    fn task_init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    /// Called for each task from parent process after fork (2) is complete.
    ///
    ///  Due to the fact that slurmd does not exec any tasks until all tasks
    ///  have completed fork (2), this call is guaranteed to run before the user
    ///  task is executed. (remote context only)
    fn task_post_fork(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    /// Called for each task as its exit status is collected by Slurm. (remote context only)
    fn task_exit(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    /// Called at the same time as the job epilog.
    ///
    /// If this function returns an error and the SPANK plugin that contains it
    /// is required in the plugstack.conf, the node that this is run on will be
    /// drained.
    fn job_epilog(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    /// Called in slurmd when the daemon is shut down.
    fn slurmd_exit(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    /// Called once just before slurmstepd exits in remote context. In local
    /// context, called before srun exits.
    fn exit(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        Ok(())
    }

    /// Called each time an Err Result is returned from a SPANK callback
    ///
    /// The default implementation logs errors through SPANK along with their
    /// causes.
    fn report_error(&self, error: &dyn Error) {
        // TODO: use error iterators once they're stable
        let mut report = error.to_string();
        let mut error = error;
        while let Some(source) = error.source() {
            report.push_str(&format!(": {}", source));
            error = source;
        }
        error!("{}", &report);
    }

    /// Called before the first callback from SPANK
    ///
    /// The default implementation configures a tracing Subscriber.
    fn setup(&self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        let default_level = match spank.context()? {
            Context::Local | Context::Allocator => "error",
            _ => "debug",
        };
        let filter_layer =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
        let fmt_layer = layer()
            .with_ansi(false)
            .event_format(SpankTraceFormatter {})
            .with_writer(SpankTraceWriter {});
        Registry::default()
            .with(filter_layer)
            .with(fmt_layer)
            .init();
        Ok(())
    }
}

struct SpankTraceFormatter;

impl<S, N> FormatEvent<S, N> for SpankTraceFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer,
        event: &Event<'_>,
    ) -> fmt::Result {
        // Write level
        let level = *event.metadata().level();
        write!(writer, "{}: ", level.to_string().to_lowercase())?;

        // Write spans and fields of each span
        ctx.visit_spans(|span| {
            write!(writer, "{}", span.name())?;

            let ext = span.extensions();

            // `FormattedFields` is a a formatted representation of the span's
            // fields, which is stored in its extensions by the `fmt` layer's
            // `new_span` method. The fields will have been formatted
            // by the same field formatter that's provided to the event
            // formatter in the `FmtContext`.
            let fields = &ext
                .get::<FormattedFields<N>>()
                .expect("will never be `None`");

            if !fields.is_empty() {
                write!(writer, "{{{}}}", fields)?;
            }
            write!(writer, ": ")?;

            Ok(())
        })?;

        // Write fields on the event
        ctx.field_format().format_fields(writer, event)
    }
}

struct SpankTraceWriter {}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SpankTraceWriter {
    type Writer = Self;

    fn make_writer(&self) -> Self::Writer {
        Self {}
    }
}

impl std::io::Write for SpankTraceWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let c_string = CString::new(buf)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?;

        unsafe {
            spank_sys::slurm_info(FORMAT_STRING.as_ptr() as *const c_char, c_string.as_ptr())
        };

        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Copy, Clone, PartialEq, IntoPrimitive)]
#[repr(u32)]
enum SpankItem {
    JobGid = spank_sys::spank_item_S_JOB_GID,
    JobUid = spank_sys::spank_item_S_JOB_UID,
    JobId = spank_sys::spank_item_S_JOB_ID,
    JobStepid = spank_sys::spank_item_S_JOB_STEPID,
    JobNnodes = spank_sys::spank_item_S_JOB_NNODES,
    JobNodeid = spank_sys::spank_item_S_JOB_NODEID,
    JobLocalTaskCount = spank_sys::spank_item_S_JOB_LOCAL_TASK_COUNT,
    JobTotalTaskCount = spank_sys::spank_item_S_JOB_TOTAL_TASK_COUNT,
    JobNcpus = spank_sys::spank_item_S_JOB_NCPUS,
    JobArgv = spank_sys::spank_item_S_JOB_ARGV,
    JobEnv = spank_sys::spank_item_S_JOB_ENV,
    TaskId = spank_sys::spank_item_S_TASK_ID,
    TaskGlobalId = spank_sys::spank_item_S_TASK_GLOBAL_ID,
    TaskExitStatus = spank_sys::spank_item_S_TASK_EXIT_STATUS,
    TaskPid = spank_sys::spank_item_S_TASK_PID,
    JobPidToGlobalId = spank_sys::spank_item_S_JOB_PID_TO_GLOBAL_ID,
    JobPidToLocalId = spank_sys::spank_item_S_JOB_PID_TO_LOCAL_ID,
    JobLocalToGlobalId = spank_sys::spank_item_S_JOB_LOCAL_TO_GLOBAL_ID,
    JobGlobalToLocalId = spank_sys::spank_item_S_JOB_GLOBAL_TO_LOCAL_ID,
    JobSupplementaryGids = spank_sys::spank_item_S_JOB_SUPPLEMENTARY_GIDS,
    SlurmVersion = spank_sys::spank_item_S_SLURM_VERSION,
    SlurmVersionMajor = spank_sys::spank_item_S_SLURM_VERSION_MAJOR,
    SlurmVersionMinor = spank_sys::spank_item_S_SLURM_VERSION_MINOR,
    SlurmVersionMicro = spank_sys::spank_item_S_SLURM_VERSION_MICRO,
    StepCpusPerTask = spank_sys::spank_item_S_STEP_CPUS_PER_TASK,
    JobAllocCores = spank_sys::spank_item_S_JOB_ALLOC_CORES,
    JobAllocMem = spank_sys::spank_item_S_JOB_ALLOC_MEM,
    StepAllocCores = spank_sys::spank_item_S_STEP_ALLOC_CORES,
    StepAllocMem = spank_sys::spank_item_S_STEP_ALLOC_MEM,
    SlurmRestartCount = spank_sys::spank_item_S_SLURM_RESTART_COUNT,
    JobArrayId = spank_sys::spank_item_S_JOB_ARRAY_ID,
    JobArrayTaskId = spank_sys::spank_item_S_JOB_ARRAY_TASK_ID,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, IntoPrimitive, FromPrimitive)]
#[repr(u32)]
/// Errors returned by the underlying SPANK API
pub enum SpankApiError {
    #[num_enum(default)]
    Generic = spank_sys::slurm_err_t_ESPANK_ERROR,
    BadArg = spank_sys::slurm_err_t_ESPANK_BAD_ARG,
    NotTask = spank_sys::slurm_err_t_ESPANK_NOT_TASK,
    EnvExists = spank_sys::slurm_err_t_ESPANK_ENV_EXISTS,
    EnvNotExist = spank_sys::slurm_err_t_ESPANK_ENV_NOEXIST,
    NoSpace = spank_sys::slurm_err_t_ESPANK_NOSPACE,
    NotRemote = spank_sys::slurm_err_t_ESPANK_NOT_REMOTE,
    NoExist = spank_sys::slurm_err_t_ESPANK_NOEXIST,
    NotExecd = spank_sys::slurm_err_t_ESPANK_NOT_EXECD,
    NotAvail = spank_sys::slurm_err_t_ESPANK_NOT_AVAIL,
    NotLocal = spank_sys::slurm_err_t_ESPANK_NOT_LOCAL,
}

impl Error for SpankApiError {}

impl fmt::Display for SpankApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cerr = unsafe { CStr::from_ptr(spank_sys::spank_strerror(*self as u32)) };

        if let Ok(err) = cerr.to_str() {
            write!(f, "{}", err)
        } else {
            write!(f, "Unknown Error")
        }
    }
}

impl Error for SpankError {}

#[derive(Debug, Clone)]
/// Main Error enum for interfaces provided by this crate
pub enum SpankError {
    CStringError(String),
    EnvExists(String),
    IdNotFound(u32),
    PidNotFound(pid_t),
    SpankAPI(String, SpankApiError),
    Utf8Error(String),
    Overflow(usize),
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
    fn from_spank_item(name: &str, arg: SpankItem, err: u32) -> SpankError {
        SpankError::SpankAPI(format!("{}({:?})", name, arg), SpankApiError::from(err))
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
            SpankError::Overflow(u) => write!(f, "Integer overflow: {}", u),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, IntoPrimitive, TryFromPrimitive)]
#[repr(u32)]
/// Context in which a plugin is loaded during a Slurm job
pub enum Context {
    // We dont represent error here, as errors are better embedded in Results
    Local = spank_sys::spank_context_S_CTX_LOCAL,
    Remote = spank_sys::spank_context_S_CTX_REMOTE,
    Allocator = spank_sys::spank_context_S_CTX_ALLOCATOR,
    Slurmd = spank_sys::spank_context_S_CTX_SLURMD,
    JobScript = spank_sys::spank_context_S_CTX_JOB_SCRIPT,
}

/// SPANK plugin command-line option that can be registered with
/// SpankHandle::register_option
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
