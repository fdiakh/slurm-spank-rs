use byte_strings::concat_bytes;
use enum_primitive::*;
use lazy_static::lazy_static;
use num::FromPrimitive;
use std::collections::HashMap;
use std::convert::TryInto;
use std::error::Error;
use std::ffi::{CStr, CString};
use std::fmt;
use std::fmt::Display;
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::slice;
use std::sync::Mutex;

mod bindings;

#[repr(C)]
pub struct StaticCStr(*const u8);
unsafe impl Sync for StaticCStr {}

unsafe fn parse_c_spank_args<'a>(
    ac: c_int,
    argv: *const *const c_char,
) -> Result<Vec<String>, String> {
    let count = ac
        .try_into()
        .map_err(|_| format!("Invalid argument count {}", ac))?;

    slice::from_raw_parts(argv, count)
        .iter()
        .map(|arg| CStr::from_ptr(*arg).to_str().map(|s| s.to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

#[derive(Default)]
struct OptionCache {
    options: Vec<String>,
    values: HashMap<String, Vec<Option<String>>>,
}

pub struct SpankHandle<'a> {
    spank: bindings::spank_t,
    opt_cache: &'a mut OptionCache,
}

impl<'a> SpankHandle<'a> {
    pub fn context(&self) -> Result<Context, SpankError> {
        let ctx = unsafe { bindings::spank_context() };
        Context::from_u32(ctx).ok_or(SpankError::Generic)
    }

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
            e => Err(SpankError::from_u32(e).unwrap_or(SpankError::Generic)),
        }
    }

    pub fn get_option_value(&self, name: &str) -> Option<&str> {
        if let Some(values) = self.opt_cache.values.get(name) {
            if let Some(value) = values.get(0) {
                value.as_deref()
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn get_option_values(&self, name: &str) -> Option<Vec<&str>> {
        if let Some(values) = self.opt_cache.values.get(name) {
            values
                .iter()
                .map(|o| o.as_deref().ok_or(()))
                .collect::<Result<Vec<&str>, ()>>()
                .ok()
        } else {
            None
        }
    }

    pub fn get_option_count(&self, name: &str) -> usize {
        if let Some(values) = self.opt_cache.values.get(name) {
            values.len()
        } else {
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn spank_opt_callback(val: c_int, optarg: *const c_char, _remote: c_int) -> c_int {
    let mut opt_cache = match OPTIONS.lock() {
        Ok(opt_cache) => opt_cache,
        // TODO: Log err
        Err(_) => return bindings::spank_err_ESPANK_ERROR as c_int,
    };

    let name = match opt_cache.options.get(val as usize) {
        Some(name) => name.clone(),
        // TODO: Log err
        _ => return bindings::spank_err_ESPANK_ERROR as c_int,
    };

    let optarg = if optarg == ptr::null() {
        None
    } else {
        let cstr = unsafe { CStr::from_ptr(optarg) };
        match cstr.to_str() {
            Ok(cstr) => Some(cstr.to_string()),
            // TODO: Log verb
            _ => return bindings::spank_err_ESPANK_ERROR as c_int,
        }
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
                    let mut plugin = match PLUGIN.lock() {
                        Ok(plugin) => plugin,
                        Err(_) => return bindings::spank_err_ESPANK_ERROR as c_int,
                    };

                    let mut opt_cache = match OPTIONS.lock() {
                        Ok(cache) => cache,
                        Err(_) => return bindings::spank_err_ESPANK_ERROR as c_int,
                    };

                    let args = match unsafe { parse_c_spank_args(ac, argv) } {
                        Ok(args) => args,
                        // FIXME Add some logging that the string was invalid
                        Err(_) => return bindings::spank_err_ESPANK_ERROR as c_int,
                    };

                    //FIXME isolate the footer part so that we don't have to use an _ prefix
                    match plugin.$rust_spank_cb(
                        &mut SpankHandle {
                            spank: spank,
                            opt_cache: &mut opt_cache,
                        },
                        args,
                    ) {
                        Ok(()) => bindings::spank_err_ESPANK_SUCCESS as c_int,
                        Err(err) => -(err as c_int),
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
    fn init(&mut self, spank: &mut SpankHandle, args: Vec<String>) -> Result<(), SpankError> {
        Ok(())
    }
    fn job_prolog(&mut self, spank: &mut SpankHandle, args: Vec<String>) -> Result<(), SpankError> {
        Ok(())
    }
    fn init_post_opt(
        &mut self,
        spank: &mut SpankHandle,
        args: Vec<String>,
    ) -> Result<(), SpankError> {
        Ok(())
    }
    fn local_user_init(
        &mut self,
        spank: &mut SpankHandle,
        args: Vec<String>,
    ) -> Result<(), SpankError> {
        Ok(())
    }
    fn user_init(&mut self, spank: &mut SpankHandle, args: Vec<String>) -> Result<(), SpankError> {
        Ok(())
    }
    fn task_init_privileged(
        &mut self,
        spank: &mut SpankHandle,
        args: Vec<String>,
    ) -> Result<(), SpankError> {
        Ok(())
    }
    fn task_init(&mut self, spank: &mut SpankHandle, args: Vec<String>) -> Result<(), SpankError> {
        Ok(())
    }
    fn task_post_fork(
        &mut self,
        spank: &mut SpankHandle,
        args: Vec<String>,
    ) -> Result<(), SpankError> {
        Ok(())
    }
    fn task_exit(&mut self, spank: &mut SpankHandle, args: Vec<String>) -> Result<(), SpankError> {
        Ok(())
    }
    fn job_epilog(&mut self, spank: &mut SpankHandle, args: Vec<String>) -> Result<(), SpankError> {
        Ok(())
    }
    fn slurmd_exit(
        &mut self,
        spank: &mut SpankHandle,
        args: Vec<String>,
    ) -> Result<(), SpankError> {
        Ok(())
    }
    fn exit(&mut self, spank: &mut SpankHandle, args: Vec<String>) -> Result<(), SpankError> {
        Ok(())
    }
}
impl Error for SpankError {}

impl Display for SpankError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cerr = unsafe { CStr::from_ptr(bindings::spank_strerror(*self as u32)) };

        if let Ok(err) = cerr.to_str() {
            write!(f, "{}", err)
        } else {
            write!(f, "Invalid Error")
        }
    }
}

enum_from_primitive! {
    #[derive(Debug, Copy, Clone, PartialEq)]
    #[repr(u32)] // XXX: FIXME: force the type generated by bindgen
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
}

enum_from_primitive! {
    #[derive(Debug, Copy, Clone, PartialEq)]
    #[repr(u32)] // FIXME: force the type generated by bindgen
    pub enum Context {
        // We dont represent error here, as errors are better embedded in Results
        Local  = bindings::spank_context_S_CTX_LOCAL,
        Remote = bindings::spank_context_S_CTX_REMOTE,
        Allocator = bindings::spank_context_S_CTX_ALLOCATOR,
        Slurmd = bindings::spank_context_S_CTX_SLURMD,
        JobScript = bindings::spank_context_S_CTX_JOB_SCRIPT,
    }
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
    fn init(&mut self, spank: &mut SpankHandle, args: Vec<String>) -> Result<(), SpankError> {
        println!("C'est l'initialisation et les arguments sont: {:?}", args);
        println!("Context is {:?}", spank.context()?);
        self.data = 20;
        spank.register_option(SpankOption::new("opt_a").usage("c'est une option"))?;
        // Ok(vec![
        //     SpankOption::new("opt_a"),
        //     SpankOption::new("opt_b").has_arg("val"),
        // ])
        Ok(())
    }

    fn exit(&mut self, spank: &mut SpankHandle, _args: Vec<String>) -> Result<(), SpankError> {
        println!("Data is {:?}", self.data);
        println!("CB returned {:?}", spank.get_option_value("opt_a"));
        Ok(())
    }
}

SPANK_PLUGIN!(b"toto", 0x130502, TestSpank);
