use std::collections::{btree_map::Values, BTreeMap};
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

pub const MAX_FDS_PER_PROCESS: usize = 256;

pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;

pub const FILETYPE_UNKNOWN: u8 = 0;
pub const FILETYPE_CHARACTER_DEVICE: u8 = 2;
pub const FILETYPE_DIRECTORY: u8 = 3;
pub const FILETYPE_REGULAR_FILE: u8 = 4;
pub const FILETYPE_PIPE: u8 = 6;
pub const FILETYPE_SYMBOLIC_LINK: u8 = 7;

pub type FdResult<T> = Result<T, FdTableError>;
pub type SharedFileDescription = Arc<FileDescription>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FdTableError {
    code: &'static str,
    message: String,
}

impl FdTableError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    fn bad_file_descriptor(fd: u32) -> Self {
        Self {
            code: "EBADF",
            message: format!("bad file descriptor {fd}"),
        }
    }

    fn too_many_open_files() -> Self {
        Self {
            code: "EMFILE",
            message: String::from("too many open files"),
        }
    }
}

impl fmt::Display for FdTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for FdTableError {}

#[derive(Debug)]
pub struct FileDescription {
    id: u64,
    path: String,
    cursor: AtomicU64,
    flags: u32,
    ref_count: AtomicUsize,
}

impl FileDescription {
    pub fn new(id: u64, path: impl Into<String>, flags: u32) -> Self {
        Self::with_ref_count(id, path, flags, 1)
    }

    pub fn with_ref_count(id: u64, path: impl Into<String>, flags: u32, ref_count: usize) -> Self {
        Self {
            id,
            path: path.into(),
            cursor: AtomicU64::new(0),
            flags,
            ref_count: AtomicUsize::new(ref_count),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn cursor(&self) -> u64 {
        self.cursor.load(Ordering::SeqCst)
    }

    pub fn set_cursor(&self, cursor: u64) {
        self.cursor.store(cursor, Ordering::SeqCst);
    }

    pub fn flags(&self) -> u32 {
        self.flags
    }

    pub fn ref_count(&self) -> usize {
        self.ref_count.load(Ordering::SeqCst)
    }

    pub fn increment_ref_count(&self) -> usize {
        self.ref_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn decrement_ref_count(&self) -> usize {
        let mut current = self.ref_count.load(Ordering::SeqCst);
        loop {
            let next = current.saturating_sub(1);
            match self
                .ref_count
                .compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return next,
                Err(observed) => current = observed,
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct FdEntry {
    pub fd: u32,
    pub description: SharedFileDescription,
    pub rights: u64,
    pub filetype: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FdStat {
    pub filetype: u8,
    pub flags: u32,
    pub rights: u64,
}

#[derive(Debug, Clone)]
pub struct StdioOverride {
    pub description: SharedFileDescription,
    pub filetype: u8,
}

#[derive(Debug, Clone)]
struct DescriptionFactory {
    next_description_id: Arc<AtomicU64>,
}

impl DescriptionFactory {
    fn new(starting_id: u64) -> Self {
        Self {
            next_description_id: Arc::new(AtomicU64::new(starting_id)),
        }
    }

    fn allocate(&self, path: &str, flags: u32) -> SharedFileDescription {
        let next_id = self.next_description_id.fetch_add(1, Ordering::SeqCst);
        Arc::new(FileDescription::new(next_id, path, flags))
    }
}

#[derive(Debug, Clone)]
pub struct ProcessFdTable {
    entries: BTreeMap<u32, FdEntry>,
    next_fd: u32,
    alloc_desc: DescriptionFactory,
}

impl ProcessFdTable {
    fn new(alloc_desc: DescriptionFactory) -> Self {
        Self {
            entries: BTreeMap::new(),
            next_fd: 3,
            alloc_desc,
        }
    }

    pub fn init_stdio(
        &mut self,
        stdin_desc: SharedFileDescription,
        stdout_desc: SharedFileDescription,
        stderr_desc: SharedFileDescription,
    ) {
        self.entries.insert(
            0,
            FdEntry {
                fd: 0,
                description: stdin_desc,
                rights: 0,
                filetype: FILETYPE_CHARACTER_DEVICE,
            },
        );
        self.entries.insert(
            1,
            FdEntry {
                fd: 1,
                description: stdout_desc,
                rights: 0,
                filetype: FILETYPE_CHARACTER_DEVICE,
            },
        );
        self.entries.insert(
            2,
            FdEntry {
                fd: 2,
                description: stderr_desc,
                rights: 0,
                filetype: FILETYPE_CHARACTER_DEVICE,
            },
        );
    }

    pub fn init_stdio_with_types(
        &mut self,
        stdin_desc: SharedFileDescription,
        stdin_type: u8,
        stdout_desc: SharedFileDescription,
        stdout_type: u8,
        stderr_desc: SharedFileDescription,
        stderr_type: u8,
    ) {
        stdin_desc.increment_ref_count();
        stdout_desc.increment_ref_count();
        stderr_desc.increment_ref_count();
        self.entries.insert(
            0,
            FdEntry {
                fd: 0,
                description: stdin_desc,
                rights: 0,
                filetype: stdin_type,
            },
        );
        self.entries.insert(
            1,
            FdEntry {
                fd: 1,
                description: stdout_desc,
                rights: 0,
                filetype: stdout_type,
            },
        );
        self.entries.insert(
            2,
            FdEntry {
                fd: 2,
                description: stderr_desc,
                rights: 0,
                filetype: stderr_type,
            },
        );
    }

    pub fn open(&mut self, path: &str, flags: u32) -> FdResult<u32> {
        self.open_with_filetype(path, flags, FILETYPE_REGULAR_FILE)
    }

    pub fn open_with_filetype(&mut self, path: &str, flags: u32, filetype: u8) -> FdResult<u32> {
        let fd = self.allocate_fd()?;
        let description = self.alloc_desc.allocate(path, flags);
        self.entries.insert(
            fd,
            FdEntry {
                fd,
                description,
                rights: 0,
                filetype,
            },
        );
        Ok(fd)
    }

    pub fn open_with(
        &mut self,
        description: SharedFileDescription,
        filetype: u8,
        target_fd: Option<u32>,
    ) -> FdResult<u32> {
        let fd = match target_fd {
            Some(fd) => {
                validate_fd_bounds(fd)?;
                fd
            }
            None => self.allocate_fd()?,
        };
        description.increment_ref_count();
        self.entries.insert(
            fd,
            FdEntry {
                fd,
                description,
                rights: 0,
                filetype,
            },
        );
        Ok(fd)
    }

    pub fn get(&self, fd: u32) -> Option<&FdEntry> {
        self.entries.get(&fd)
    }

    pub fn close(&mut self, fd: u32) -> bool {
        let Some(entry) = self.entries.remove(&fd) else {
            return false;
        };
        entry.description.decrement_ref_count();
        true
    }

    pub fn dup(&mut self, fd: u32) -> FdResult<u32> {
        let entry = self
            .entries
            .get(&fd)
            .cloned()
            .ok_or_else(|| FdTableError::bad_file_descriptor(fd))?;
        let new_fd = self.allocate_fd()?;
        entry.description.increment_ref_count();
        self.entries.insert(
            new_fd,
            FdEntry {
                fd: new_fd,
                description: entry.description,
                rights: entry.rights,
                filetype: entry.filetype,
            },
        );
        Ok(new_fd)
    }

    pub fn dup2(&mut self, old_fd: u32, new_fd: u32) -> FdResult<()> {
        let entry = self
            .entries
            .get(&old_fd)
            .cloned()
            .ok_or_else(|| FdTableError::bad_file_descriptor(old_fd))?;
        validate_fd_bounds(new_fd)?;
        if old_fd == new_fd {
            return Ok(());
        }

        if self.entries.contains_key(&new_fd) {
            self.close(new_fd);
        }

        entry.description.increment_ref_count();
        self.entries.insert(
            new_fd,
            FdEntry {
                fd: new_fd,
                description: entry.description,
                rights: entry.rights,
                filetype: entry.filetype,
            },
        );
        Ok(())
    }

    pub fn stat(&self, fd: u32) -> FdResult<FdStat> {
        let entry = self
            .entries
            .get(&fd)
            .ok_or_else(|| FdTableError::bad_file_descriptor(fd))?;
        Ok(FdStat {
            filetype: entry.filetype,
            flags: entry.description.flags(),
            rights: entry.rights,
        })
    }

    pub fn fork(&self) -> Self {
        let mut child = Self::new(self.alloc_desc.clone());
        child.next_fd = self.next_fd;

        for (fd, entry) in &self.entries {
            entry.description.increment_ref_count();
            child.entries.insert(
                *fd,
                FdEntry {
                    fd: *fd,
                    description: Arc::clone(&entry.description),
                    rights: entry.rights,
                    filetype: entry.filetype,
                },
            );
        }

        child
    }

    pub fn close_all(&mut self) {
        let fds: Vec<u32> = self.entries.keys().copied().collect();
        for fd in fds {
            self.close(fd);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> Values<'_, u32, FdEntry> {
        self.entries.values()
    }

    fn allocate_fd(&mut self) -> FdResult<u32> {
        if self.entries.len() >= MAX_FDS_PER_PROCESS {
            return Err(FdTableError::too_many_open_files());
        }

        let start = usize::try_from(self.next_fd).unwrap_or(0) % MAX_FDS_PER_PROCESS;
        for offset in 0..MAX_FDS_PER_PROCESS {
            let candidate = ((start + offset) % MAX_FDS_PER_PROCESS) as u32;
            if !self.entries.contains_key(&candidate) {
                self.next_fd = candidate.saturating_add(1);
                return Ok(candidate);
            }
        }

        Err(FdTableError::too_many_open_files())
    }
}

fn validate_fd_bounds(fd: u32) -> FdResult<()> {
    if fd as usize >= MAX_FDS_PER_PROCESS {
        return Err(FdTableError::bad_file_descriptor(fd));
    }
    Ok(())
}

impl<'a> IntoIterator for &'a ProcessFdTable {
    type Item = &'a FdEntry;
    type IntoIter = Values<'a, u32, FdEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.values()
    }
}

#[derive(Debug, Clone)]
pub struct FdTableManager {
    tables: BTreeMap<u32, ProcessFdTable>,
    alloc_desc: DescriptionFactory,
}

impl Default for FdTableManager {
    fn default() -> Self {
        Self {
            tables: BTreeMap::new(),
            alloc_desc: DescriptionFactory::new(1),
        }
    }
}

impl FdTableManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(&mut self, pid: u32) -> &mut ProcessFdTable {
        let mut table = ProcessFdTable::new(self.alloc_desc.clone());
        table.init_stdio(
            self.alloc_desc.allocate("/dev/stdin", O_RDONLY),
            self.alloc_desc.allocate("/dev/stdout", O_WRONLY),
            self.alloc_desc.allocate("/dev/stderr", O_WRONLY),
        );
        self.remove(pid);
        self.tables.insert(pid, table);
        self.tables
            .get_mut(&pid)
            .expect("newly created FD table should be stored")
    }

    pub fn create_with_stdio(
        &mut self,
        pid: u32,
        stdin_override: Option<StdioOverride>,
        stdout_override: Option<StdioOverride>,
        stderr_override: Option<StdioOverride>,
    ) -> &mut ProcessFdTable {
        let mut table = ProcessFdTable::new(self.alloc_desc.clone());
        let stdin_desc = stdin_override
            .as_ref()
            .map(|entry| Arc::clone(&entry.description))
            .unwrap_or_else(|| self.alloc_desc.allocate("/dev/stdin", O_RDONLY));
        let stdout_desc = stdout_override
            .as_ref()
            .map(|entry| Arc::clone(&entry.description))
            .unwrap_or_else(|| self.alloc_desc.allocate("/dev/stdout", O_WRONLY));
        let stderr_desc = stderr_override
            .as_ref()
            .map(|entry| Arc::clone(&entry.description))
            .unwrap_or_else(|| self.alloc_desc.allocate("/dev/stderr", O_WRONLY));

        table.init_stdio_with_types(
            stdin_desc,
            stdin_override
                .as_ref()
                .map(|entry| entry.filetype)
                .unwrap_or(FILETYPE_CHARACTER_DEVICE),
            stdout_desc,
            stdout_override
                .as_ref()
                .map(|entry| entry.filetype)
                .unwrap_or(FILETYPE_CHARACTER_DEVICE),
            stderr_desc,
            stderr_override
                .as_ref()
                .map(|entry| entry.filetype)
                .unwrap_or(FILETYPE_CHARACTER_DEVICE),
        );
        self.remove(pid);
        self.tables.insert(pid, table);
        self.tables
            .get_mut(&pid)
            .expect("newly created FD table should be stored")
    }

    pub fn fork(&mut self, parent_pid: u32, child_pid: u32) -> &mut ProcessFdTable {
        if !self.tables.contains_key(&parent_pid) {
            return self.create(child_pid);
        }

        let child = self
            .tables
            .get(&parent_pid)
            .expect("parent table presence was checked")
            .fork();
        self.remove(child_pid);
        self.tables.insert(child_pid, child);
        self.tables
            .get_mut(&child_pid)
            .expect("forked FD table should be stored")
    }

    pub fn get(&self, pid: u32) -> Option<&ProcessFdTable> {
        self.tables.get(&pid)
    }

    pub fn get_mut(&mut self, pid: u32) -> Option<&mut ProcessFdTable> {
        self.tables.get_mut(&pid)
    }

    pub fn has(&self, pid: u32) -> bool {
        self.tables.contains_key(&pid)
    }

    pub fn len(&self) -> usize {
        self.tables.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    pub fn total_open_fds(&self) -> usize {
        self.tables.values().map(ProcessFdTable::len).sum()
    }

    pub fn pids(&self) -> Vec<u32> {
        self.tables.keys().copied().collect()
    }

    pub fn remove(&mut self, pid: u32) {
        if let Some(mut table) = self.tables.remove(&pid) {
            table.close_all();
        }
    }
}
