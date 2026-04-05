// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

// spell-checker:ignore (vars) Passwd cstr fnam gecos ngroups egid

//! Get password/group file entry
//!
//! wasmVM: This module has two implementations — the original libc-based one
//! for unix, and a WASI-compatible one using host_user FFI.

// ============================================================
// Unix implementation (original)
// ============================================================
#[cfg(unix)]
mod unix_impl {
    #[cfg(any(target_os = "freebsd", target_vendor = "apple"))]
    use libc::time_t;
    use libc::{c_char, c_int, gid_t, uid_t};
    use libc::{getgrgid, getgrnam, getgroups};
    use libc::{getpwnam, getpwuid, group, passwd};

    use std::ffi::{CStr, CString};
    use std::io::Error as IOError;
    use std::io::ErrorKind;
    use std::io::Result as IOResult;
    use std::ptr;
    use std::sync::Mutex;

    unsafe extern "C" {
        fn getgrouplist(
            name: *const c_char,
            gid: gid_t,
            groups: *mut gid_t,
            ngroups: *mut c_int,
        ) -> c_int;
    }

    pub fn get_groups() -> IOResult<Vec<gid_t>> {
        let mut groups = Vec::new();
        loop {
            let ngroups = match unsafe { getgroups(0, ptr::null_mut()) } {
                -1 => return Err(IOError::last_os_error()),
                0 => return Ok(Vec::new()),
                n => n,
            };
            groups.resize(ngroups.try_into().unwrap(), 0);
            let res = unsafe { getgroups(ngroups, groups.as_mut_ptr()) };
            if res == -1 {
                let err = IOError::last_os_error();
                if err.raw_os_error() == Some(libc::EINVAL) {
                    // Number of groups has increased, retry
                } else {
                    return Err(err);
                }
            } else {
                groups.truncate(res.try_into().unwrap());
                return Ok(groups);
            }
        }
    }

    #[cfg(all(unix, not(target_os = "redox"), feature = "process"))]
    pub fn get_groups_gnu(arg_id: Option<u32>) -> IOResult<Vec<gid_t>> {
        let groups = get_groups()?;
        let egid = arg_id.unwrap_or_else(crate::features::process::getegid);
        Ok(sort_groups(groups, egid))
    }

    #[cfg(all(unix, not(target_os = "redox"), feature = "process"))]
    pub(crate) fn sort_groups(mut groups: Vec<gid_t>, egid: gid_t) -> Vec<gid_t> {
        if let Some(index) = groups.iter().position(|&x| x == egid) {
            groups[..=index].rotate_right(1);
        } else {
            groups.insert(0, egid);
        }
        groups
    }

    #[derive(Clone, Debug)]
    pub struct Passwd {
        pub name: String,
        pub uid: uid_t,
        pub gid: gid_t,
        pub user_info: Option<String>,
        pub user_shell: Option<String>,
        pub user_dir: Option<String>,
        #[expect(clippy::struct_field_names)]
        pub user_passwd: Option<String>,
        #[cfg(any(target_os = "freebsd", target_vendor = "apple"))]
        pub user_access_class: Option<String>,
        #[cfg(any(target_os = "freebsd", target_vendor = "apple"))]
        #[expect(clippy::struct_field_names)]
        pub passwd_change_time: time_t,
        #[cfg(any(target_os = "freebsd", target_vendor = "apple"))]
        pub expiration: time_t,
    }

    fn cstr2string(ptr: *const c_char) -> Option<String> {
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() })
        }
    }

    impl Passwd {
        unsafe fn from_raw(raw: passwd) -> Self {
            Self {
                name: cstr2string(raw.pw_name).expect("passwd without name"),
                uid: raw.pw_uid,
                gid: raw.pw_gid,
                #[cfg(not(all(
                    target_os = "android",
                    any(target_arch = "x86", target_arch = "arm")
                )))]
                user_info: cstr2string(raw.pw_gecos),
                #[cfg(all(target_os = "android", any(target_arch = "x86", target_arch = "arm")))]
                user_info: None,
                user_shell: cstr2string(raw.pw_shell),
                user_dir: cstr2string(raw.pw_dir),
                user_passwd: cstr2string(raw.pw_passwd),
                #[cfg(any(target_os = "freebsd", target_vendor = "apple"))]
                user_access_class: cstr2string(raw.pw_class),
                #[cfg(any(target_os = "freebsd", target_vendor = "apple"))]
                passwd_change_time: raw.pw_change,
                #[cfg(any(target_os = "freebsd", target_vendor = "apple"))]
                expiration: raw.pw_expire,
            }
        }

        pub fn belongs_to(&self) -> Vec<gid_t> {
            let mut ngroups: c_int = 8;
            let mut ngroups_old: c_int;
            let mut groups = vec![0; ngroups.try_into().unwrap()];
            let name = CString::new(self.name.as_bytes()).unwrap();
            loop {
                ngroups_old = ngroups;
                if unsafe {
                    getgrouplist(
                        name.as_ptr(),
                        self.gid,
                        groups.as_mut_ptr(),
                        &raw mut ngroups,
                    )
                } == -1
                {
                    if ngroups == ngroups_old {
                        ngroups *= 2;
                    }
                    groups.resize(ngroups.try_into().unwrap(), 0);
                } else {
                    break;
                }
            }
            let ngroups = ngroups.try_into().unwrap();
            assert!(ngroups <= groups.len());
            groups.truncate(ngroups);
            groups
        }
    }

    #[derive(Clone, Debug)]
    pub struct Group {
        pub name: String,
        pub gid: gid_t,
    }

    impl Group {
        unsafe fn from_raw(raw: group) -> Self {
            Self {
                name: cstr2string(raw.gr_name).expect("group without name"),
                gid: raw.gr_gid,
            }
        }
    }

    pub trait Locate<K> {
        fn locate(key: K) -> IOResult<Self>
        where
            Self: ::std::marker::Sized;
    }

    static PW_LOCK: Mutex<()> = Mutex::new(());

    macro_rules! f {
        ($fnam:ident, $fid:ident, $t:ident, $st:ident) => {
            impl Locate<$t> for $st {
                fn locate(k: $t) -> IOResult<Self> {
                    let _guard = PW_LOCK.lock();
                    unsafe {
                        let data = $fid(k);
                        if !data.is_null() {
                            Ok($st::from_raw(ptr::read(data.cast_const())))
                        } else {
                            Err(IOError::new(
                                ErrorKind::NotFound,
                                format!("No such id: {k}"),
                            ))
                        }
                    }
                }
            }

            impl<'a> Locate<&'a str> for $st {
                fn locate(k: &'a str) -> IOResult<Self> {
                    let _guard = PW_LOCK.lock();
                    unsafe {
                        let cstring = CString::new(k)?;
                        let data = $fnam(cstring.as_ptr());
                        if !data.is_null() {
                            return Ok($st::from_raw(ptr::read(data.cast_const())));
                        }
                        if let Ok(id) = k.parse::<$t>() {
                            let data = $fid(id);
                            if !data.is_null() {
                                Ok($st::from_raw(ptr::read(data.cast_const())))
                            } else {
                                Err(IOError::new(
                                    ErrorKind::NotFound,
                                    format!("No such id: {id}"),
                                ))
                            }
                        } else {
                            Err(IOError::new(ErrorKind::NotFound, format!("Not found: {k}")))
                        }
                    }
                }
            }
        };
    }

    f!(getpwnam, getpwuid, uid_t, Passwd);
    f!(getgrnam, getgrgid, gid_t, Group);

    #[inline]
    pub fn uid2usr(id: uid_t) -> IOResult<String> {
        Passwd::locate(id).map(|p| p.name)
    }

    #[inline]
    pub fn gid2grp(id: gid_t) -> IOResult<String> {
        Group::locate(id).map(|p| p.name)
    }

    #[inline]
    pub fn usr2uid(name: &str) -> IOResult<uid_t> {
        Passwd::locate(name).map(|p| p.uid)
    }

    #[inline]
    pub fn usr2gid(name: &str) -> IOResult<gid_t> {
        Passwd::locate(name).map(|p| p.gid)
    }

    #[inline]
    pub fn grp2gid(name: &str) -> IOResult<gid_t> {
        Group::locate(name).map(|p| p.gid)
    }
}

// ============================================================
// WASI implementation (wasmVM: uses host_user FFI)
// ============================================================
#[cfg(target_os = "wasi")]
mod wasi_impl {
    use std::io::Error as IOError;
    use std::io::ErrorKind;
    use std::io::Result as IOResult;

    /// WASI doesn't have libc uid_t/gid_t — use u32 directly.
    #[allow(non_camel_case_types)]
    pub type uid_t = u32;
    #[allow(non_camel_case_types)]
    pub type gid_t = u32;

    // FFI bindings for host_user WASM import module
    #[link(wasm_import_module = "host_user")]
    unsafe extern "C" {
        fn getuid(ret_uid: *mut u32) -> u32;
        fn getgid(ret_gid: *mut u32) -> u32;
        fn geteuid(ret_uid: *mut u32) -> u32;
        fn getegid(ret_gid: *mut u32) -> u32;
        fn getpwuid(uid: u32, buf_ptr: *mut u8, buf_len: u32, ret_len: *mut u32) -> u32;
    }

    /// Parse a passwd string "name:passwd:uid:gid:gecos:home:shell" into fields.
    fn parse_passwd_string(s: &str) -> Option<(String, String, u32, u32, String, String, String)> {
        let fields: Vec<&str> = s.splitn(7, ':').collect();
        if fields.len() < 7 {
            return None;
        }
        let uid = fields[2].parse::<u32>().ok()?;
        let gid = fields[3].parse::<u32>().ok()?;
        Some((
            fields[0].to_string(), // name
            fields[1].to_string(), // passwd
            uid,
            gid,
            fields[4].to_string(), // gecos
            fields[5].to_string(), // home
            fields[6].to_string(), // shell
        ))
    }

    /// Call host_user.getpwuid and parse the response.
    fn lookup_pwuid(uid: u32) -> Option<Passwd> {
        let mut buf = [0u8; 512];
        let mut len: u32 = 0;
        let errno = unsafe { getpwuid(uid, buf.as_mut_ptr(), buf.len() as u32, &mut len) };
        if errno != 0 || len == 0 {
            return None;
        }
        let len = usize::try_from(len).ok()?;
        if len > buf.len() {
            return None;
        }
        let s = core::str::from_utf8(&buf[..len]).ok()?;
        let (name, passwd, pw_uid, pw_gid, gecos, home, shell) = parse_passwd_string(s)?;
        Some(Passwd {
            name,
            uid: pw_uid,
            gid: pw_gid,
            user_info: Some(gecos),
            user_shell: Some(shell),
            user_dir: Some(home),
            user_passwd: Some(passwd),
        })
    }

    /// Get the current real UID via host_user.
    fn current_uid() -> u32 {
        let mut uid: u32 = 0;
        unsafe { getuid(&mut uid) };
        uid
    }

    /// Get the current real GID via host_user.
    fn current_gid() -> u32 {
        let mut gid: u32 = 0;
        unsafe { getgid(&mut gid) };
        gid
    }

    /// Get the effective GID via host_user.
    fn current_egid() -> u32 {
        let mut gid: u32 = 0;
        unsafe { getegid(&mut gid) };
        gid
    }

    pub fn get_groups() -> IOResult<Vec<gid_t>> {
        // WASI: return just the primary group
        Ok(vec![current_gid()])
    }

    #[derive(Clone, Debug)]
    pub struct Passwd {
        pub name: String,
        pub uid: uid_t,
        pub gid: gid_t,
        pub user_info: Option<String>,
        pub user_shell: Option<String>,
        pub user_dir: Option<String>,
        pub user_passwd: Option<String>,
    }

    impl Passwd {
        pub fn belongs_to(&self) -> Vec<gid_t> {
            vec![self.gid]
        }
    }

    #[derive(Clone, Debug)]
    pub struct Group {
        pub name: String,
        pub gid: gid_t,
    }

    /// Fetch desired entry.
    pub trait Locate<K> {
        fn locate(key: K) -> IOResult<Self>
        where
            Self: ::std::marker::Sized;
    }

    // Locate by uid_t for Passwd
    impl Locate<uid_t> for Passwd {
        fn locate(uid: uid_t) -> IOResult<Self> {
            lookup_pwuid(uid).ok_or_else(|| {
                IOError::new(ErrorKind::NotFound, format!("No such id: {uid}"))
            })
        }
    }

    // Locate by name (str) for Passwd
    impl<'a> Locate<&'a str> for Passwd {
        fn locate(name: &'a str) -> IOResult<Self> {
            // Try parsing as numeric UID first
            if let Ok(uid) = name.parse::<u32>() {
                return Passwd::locate(uid);
            }
            // Try known UIDs: 0 (root) and current uid
            for uid in [0, current_uid()] {
                if let Some(pw) = lookup_pwuid(uid) {
                    if pw.name == name {
                        return Ok(pw);
                    }
                }
            }
            Err(IOError::new(ErrorKind::NotFound, format!("Not found: {name}")))
        }
    }

    // Locate by gid_t for Group
    impl Locate<gid_t> for Group {
        fn locate(gid: gid_t) -> IOResult<Self> {
            // Synthetic group entries: try to derive from passwd if gid matches
            let name = match gid {
                0 => "root".to_string(),
                _ => {
                    // Try current user's passwd entry
                    let cur_uid = current_uid();
                    if let Some(pw) = lookup_pwuid(cur_uid) {
                        if pw.gid == gid {
                            pw.name.clone()
                        } else {
                            format!("group{gid}")
                        }
                    } else {
                        format!("group{gid}")
                    }
                }
            };
            Ok(Group { name, gid })
        }
    }

    // Locate by name (str) for Group
    impl<'a> Locate<&'a str> for Group {
        fn locate(name: &'a str) -> IOResult<Self> {
            // Try parsing as numeric GID first
            if let Ok(gid) = name.parse::<u32>() {
                return Group::locate(gid);
            }
            // Well-known group names
            if name == "root" || name == "wheel" {
                return Ok(Group {
                    name: name.to_string(),
                    gid: 0,
                });
            }
            // Try to match current user's primary group
            let cur_uid = current_uid();
            if let Some(pw) = lookup_pwuid(cur_uid) {
                if pw.name == name {
                    return Ok(Group {
                        name: name.to_string(),
                        gid: pw.gid,
                    });
                }
            }
            Err(IOError::new(
                ErrorKind::NotFound,
                format!("Not found: {name}"),
            ))
        }
    }

    #[inline]
    pub fn uid2usr(id: uid_t) -> IOResult<String> {
        Passwd::locate(id).map(|p| p.name)
    }

    #[inline]
    pub fn gid2grp(id: gid_t) -> IOResult<String> {
        Group::locate(id).map(|p| p.name)
    }

    #[inline]
    pub fn usr2uid(name: &str) -> IOResult<uid_t> {
        Passwd::locate(name).map(|p| p.uid)
    }

    #[inline]
    pub fn usr2gid(name: &str) -> IOResult<gid_t> {
        Passwd::locate(name).map(|p| p.gid)
    }

    #[inline]
    pub fn grp2gid(name: &str) -> IOResult<gid_t> {
        Group::locate(name).map(|p| p.gid)
    }
}

// ============================================================
// Re-export the appropriate implementation
// ============================================================
#[cfg(unix)]
pub use unix_impl::*;

#[cfg(target_os = "wasi")]
pub use wasi_impl::*;

#[cfg(test)]
mod test {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn test_sort_groups() {
        assert_eq!(unix_impl::sort_groups(vec![1, 2, 3], 4), vec![4, 1, 2, 3]);
        assert_eq!(unix_impl::sort_groups(vec![1, 2, 3], 3), vec![3, 1, 2]);
        assert_eq!(unix_impl::sort_groups(vec![1, 2, 3], 2), vec![2, 1, 3]);
        assert_eq!(unix_impl::sort_groups(vec![1, 2, 3], 1), vec![1, 2, 3]);
        assert_eq!(unix_impl::sort_groups(vec![1, 2, 3], 0), vec![0, 1, 2, 3]);
    }

    #[cfg(all(unix, not(target_os = "redox"), feature = "process"))]
    #[test]
    fn test_entries_get_groups_gnu() {
        if let Ok(mut groups) = get_groups() {
            if let Some(last) = groups.pop() {
                groups.insert(0, last);
                assert_eq!(unix_impl::get_groups_gnu(Some(last)).unwrap(), groups);
            }
        }
    }
}
