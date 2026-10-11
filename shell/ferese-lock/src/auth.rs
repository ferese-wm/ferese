use std::ffi::{CStr, CString, c_char, c_int, c_void};

use zeroize::Zeroizing;

#[repr(C)]
struct Message {
    style: c_int,
    text: *const c_char,
}
#[repr(C)]
struct Response {
    text: *mut c_char,
    code: c_int,
}
#[repr(C)]
struct Conversation {
    callback: unsafe extern "C" fn(c_int, *mut *const Message, *mut *mut Response, *mut c_void) -> c_int,
    data: *mut c_void,
}
struct Credentials {
    user: CString,
    password: Zeroizing<Vec<u8>>,
    password_prompts: u8,
}
const SUCCESS: c_int = 0;
const CONV_ERR: c_int = 19;

// Linux-PAM owns successful response allocations and frees them using free().
unsafe extern "C" fn converse(
    count: c_int,
    messages: *mut *const Message,
    out: *mut *mut Response,
    data: *mut c_void,
) -> c_int {
    if count <= 0 || count > 32 || messages.is_null() || out.is_null() || data.is_null() {
        return CONV_ERR;
    }
    unsafe {
        *out = std::ptr::null_mut();
        let credentials = &mut *(data as *mut Credentials);
        let responses = libc::calloc(count as usize, std::mem::size_of::<Response>()) as *mut Response;
        if responses.is_null() {
            return CONV_ERR;
        }
        for i in 0..count as usize {
            let msg = *messages.add(i);
            let bytes = if msg.is_null() {
                None
            } else {
                match (*msg).style {
                    1 => {
                        credentials.password_prompts = credentials.password_prompts.saturating_add(1);
                        (credentials.password_prompts == 1).then_some(credentials.password.as_slice())
                    } // Never send the password as a second-factor response.
                    2 => Some(credentials.user.as_bytes_with_nul()), // PAM_PROMPT_ECHO_ON
                    3 | 4 => Some(&[][..]),                          // PAM_ERROR_MSG / PAM_TEXT_INFO
                    _ => None,
                }
            };
            if let Some(bytes) = bytes {
                if !bytes.is_empty() {
                    let allocation = libc::malloc(bytes.len()) as *mut c_char;
                    if !allocation.is_null() {
                        std::ptr::copy_nonoverlapping(bytes.as_ptr(), allocation.cast(), bytes.len());
                        (*responses.add(i)).text = allocation;
                        continue;
                    }
                } else {
                    continue;
                }
            }
            for j in 0..i {
                let ptr = (*responses.add(j)).text;
                if !ptr.is_null() {
                    libc::explicit_bzero(ptr.cast(), CStr::from_ptr(ptr).to_bytes().len());
                    libc::free(ptr.cast());
                }
            }
            libc::free(responses.cast());
            return CONV_ERR;
        }
        *out = responses;
        SUCCESS
    }
}

pub fn username() -> Result<String, String> {
    // Never trust USER/LOGNAME for the account being unlocked.
    unsafe {
        let mut entry: libc::passwd = std::mem::zeroed();
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0u8; 16384];
        let status = libc::getpwuid_r(
            libc::getuid(),
            &mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        );
        if status != 0 || result.is_null() || entry.pw_name.is_null() {
            return Err("Cannot determine the session user".into());
        }
        Ok(CStr::from_ptr(entry.pw_name).to_string_lossy().into_owned())
    }
}

pub fn available() -> bool {
    std::path::Path::new("/etc/pam.d/ferese-lock").is_file()
        && unsafe { libloading::Library::new("libpam.so.0") }.is_ok()
}

pub fn authenticate(user: String, password: Zeroizing<String>) -> bool {
    authenticate_with(&user, &password, None)
}

fn authenticate_with(user: &str, password: &str, config: Option<&CStr>) -> bool {
    if password.is_empty() || password.contains('\0') {
        return false;
    }
    let Ok(user) = CString::new(user) else {
        return false;
    };
    let mut secret = Zeroizing::new(password.as_bytes().to_vec());
    secret.push(0);
    let mut credentials = Credentials {
        user,
        password: secret,
        password_prompts: 0,
    };
    unsafe {
        let Ok(library) = libloading::Library::new("libpam.so.0") else {
            return false;
        };
        type Start = unsafe extern "C" fn(*const c_char, *const c_char, *const Conversation, *mut *mut c_void) -> c_int;
        type StartConf = unsafe extern "C" fn(
            *const c_char,
            *const c_char,
            *const Conversation,
            *const c_char,
            *mut *mut c_void,
        ) -> c_int;
        type Check = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
        let (Ok(start), Ok(auth), Ok(account), Ok(end)) = (
            library.get::<Start>(b"pam_start\0"),
            library.get::<Check>(b"pam_authenticate\0"),
            library.get::<Check>(b"pam_acct_mgmt\0"),
            library.get::<Check>(b"pam_end\0"),
        ) else {
            return false;
        };
        let conversation = Conversation {
            callback: converse,
            data: (&mut credentials as *mut Credentials).cast(),
        };
        let mut handle = std::ptr::null_mut();
        let status = if let Some(config) = config {
            let Ok(start) = library.get::<StartConf>(b"pam_start_confdir\0") else {
                return false;
            };
            start(
                c"ferese-lock".as_ptr(),
                credentials.user.as_ptr(),
                &conversation,
                config.as_ptr(),
                &mut handle,
            )
        } else {
            start(
                c"ferese-lock".as_ptr(),
                credentials.user.as_ptr(),
                &conversation,
                &mut handle,
            )
        };
        if status != SUCCESS || handle.is_null() {
            return false;
        }
        let mut status = auth(handle, 1); // PAM_DISALLOW_NULL_AUTHTOK
        if status == SUCCESS {
            status = account(handle, 0);
        }
        let ended = end(handle, status);
        status == SUCCESS && ended == SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pam_requires_authentication_and_account_permission() {
        let directory = tempfile::tempdir().unwrap();
        let path = CString::new(directory.path().as_os_str().as_encoded_bytes()).unwrap();
        for (auth, account, allowed) in [
            ("permit", "permit", true),
            ("deny", "permit", false),
            ("permit", "deny", false),
        ] {
            std::fs::write(
                directory.path().join("ferese-lock"),
                format!("auth required pam_{auth}.so\naccount required pam_{account}.so\n"),
            )
            .unwrap();
            assert_eq!(authenticate_with("test", "test-only", Some(&path)), allowed);
        }
        assert!(!authenticate_with("test", "", Some(&path)));
        assert!(!authenticate_with("test", "bad\0value", Some(&path)));
    }
    #[test]
    fn conversation_rejects_multiple_secret_prompts_and_unknown_styles() {
        let mut credentials = Credentials {
            user: CString::new("tester").unwrap(),
            password: Zeroizing::new(b"test-only\0".to_vec()),
            password_prompts: 0,
        };
        let first = Message {
            style: 1,
            text: c"Password".as_ptr(),
        };
        let mut prompts = [&first as *const Message, &first as *const Message];
        let mut responses = std::ptr::null_mut();
        unsafe {
            assert_eq!(
                converse(
                    2,
                    prompts.as_mut_ptr(),
                    &mut responses,
                    (&mut credentials as *mut Credentials).cast()
                ),
                CONV_ERR
            );
            assert!(responses.is_null());
            let unknown = Message {
                style: 99,
                text: std::ptr::null(),
            };
            prompts[0] = &unknown;
            assert_eq!(
                converse(
                    1,
                    prompts.as_mut_ptr(),
                    &mut responses,
                    (&mut credentials as *mut Credentials).cast()
                ),
                CONV_ERR
            );
            assert!(responses.is_null());
        }
    }
    #[test]
    fn current_user_comes_from_uid() {
        assert!(!username().unwrap().is_empty());
    }
}
