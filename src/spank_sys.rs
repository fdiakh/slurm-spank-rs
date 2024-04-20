#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct spank_option {
    pub name: *const ::std::os::raw::c_char,
    pub arginfo: *const ::std::os::raw::c_char,
    pub usage: *const ::std::os::raw::c_char,
    pub has_arg: ::std::os::raw::c_int,
    pub val: ::std::os::raw::c_int,
    pub cb: spank_opt_cb_f,
}

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
