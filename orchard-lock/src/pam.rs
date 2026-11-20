//! Password check through PAM. libpam is loaded at runtime (`dlopen`), so
//! building needs no PAM headers; the lock screen only ever authenticates.

use std::ffi::{c_char, c_int, c_void, CStr, CString};

const PAM_SUCCESS: c_int = 0;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_BUF_ERR: c_int = 5;
const PAM_CONV_ERR: c_int = 19;
const PAM_REFRESH_CRED: c_int = 0x0010;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type ConvFn = unsafe extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int;

#[repr(C)]
struct PamConv {
    conv: ConvFn,
    appdata_ptr: *mut c_void,
}

type PamStart = unsafe extern "C" fn(*const c_char, *const c_char, *const PamConv, *mut *mut c_void) -> c_int;
type PamCall = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;

struct Lib {
    start: PamStart,
    authenticate: PamCall,
    setcred: PamCall,
    end: PamCall,
}

fn lib() -> Result<&'static Lib, String> {
    static LIB: std::sync::OnceLock<Result<Lib, String>> = std::sync::OnceLock::new();
    LIB.get_or_init(|| unsafe {
        let h = libc::dlopen(c"libpam.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if h.is_null() {
            return Err("libpam.so.0 not found".into());
        }
        let sym = |name: &CStr| {
            let p = libc::dlsym(h, name.as_ptr());
            if p.is_null() {
                Err(format!("libpam lacks {}", name.to_string_lossy()))
            } else {
                Ok(p)
            }
        };
        Ok(Lib {
            start: std::mem::transmute::<*mut c_void, PamStart>(sym(c"pam_start")?),
            authenticate: std::mem::transmute::<*mut c_void, PamCall>(sym(c"pam_authenticate")?),
            setcred: std::mem::transmute::<*mut c_void, PamCall>(sym(c"pam_setcred")?),
            end: std::mem::transmute::<*mut c_void, PamCall>(sym(c"pam_end")?),
        })
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// Answers every prompt with the password (PAM frees the copies).
unsafe extern "C" fn conversation(
    num: c_int,
    msgs: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    data: *mut c_void,
) -> c_int {
    if num <= 0 || msgs.is_null() || resp.is_null() {
        return PAM_CONV_ERR;
    }
    let out = libc::calloc(num as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
    if out.is_null() {
        return PAM_BUF_ERR;
    }
    let password = data as *const c_char;
    for i in 0..num as isize {
        let m = *msgs.offset(i);
        if m.is_null() {
            continue;
        }
        match (*m).msg_style {
            PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON => {
                let copy = libc::strdup(password);
                if copy.is_null() {
                    for j in 0..i {
                        let r = (*out.offset(j)).resp;
                        if !r.is_null() {
                            libc::explicit_bzero(r as *mut c_void, libc::strlen(r));
                            libc::free(r as *mut c_void);
                        }
                    }
                    libc::free(out as *mut c_void);
                    return PAM_BUF_ERR;
                }
                (*out.offset(i)).resp = copy;
            }
            // info / error texts: nothing to answer
            _ => {}
        }
    }
    *resp = out;
    PAM_SUCCESS
}

/// The PAM service: Orchard's own, else swaylock's (installed on most
/// systems with a locker), else plain `login`.
fn service() -> &'static CStr {
    for (name, c) in [("orchard-lock", c"orchard-lock"), ("swaylock", c"swaylock")] {
        if std::path::Path::new("/etc/pam.d").join(name).exists() || std::path::Path::new("/usr/lib/pam.d").join(name).exists() {
            return c;
        }
    }
    c"login"
}

/// Check `password` for `user`. Blocking (pam_faildelay holds a wrong
/// password for ~2 s); run it off the UI thread. The password bytes are wiped.
pub fn authenticate(user: &str, mut password: Vec<u8>) -> Result<(), String> {
    let lib = lib()?;
    let user = CString::new(user).map_err(|_| "bad user name".to_string())?;
    password.retain(|b| *b != 0);
    password.push(0);
    let conv = PamConv {
        conv: conversation,
        appdata_ptr: password.as_ptr() as *mut c_void,
    };
    let result = unsafe {
        let mut handle: *mut c_void = std::ptr::null_mut();
        let rc = (lib.start)(service().as_ptr(), user.as_ptr(), &conv, &mut handle);
        if rc != PAM_SUCCESS || handle.is_null() {
            Err(format!("pam_start failed ({rc})"))
        } else {
            let rc = (lib.authenticate)(handle, 0);
            if rc == PAM_SUCCESS {
                // renew Kerberos tickets and the like; failure is not fatal
                let _ = (lib.setcred)(handle, PAM_REFRESH_CRED);
            }
            (lib.end)(handle, rc);
            if rc == PAM_SUCCESS {
                Ok(())
            } else {
                Err(format!("authentication failed ({rc})"))
            }
        }
    };
    wipe(&mut password);
    result
}

/// Overwrite secret bytes in a way the optimizer keeps.
pub fn wipe(bytes: &mut Vec<u8>) {
    unsafe { libc::explicit_bzero(bytes.as_mut_ptr() as *mut c_void, bytes.capacity()) };
    bytes.clear();
}
