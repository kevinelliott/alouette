//! A small SFTP server over a folder, for tests. Paths are taken as relative to the folder;
//! nothing stops `..`, so never point it at anything that matters.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version};

enum Open {
    File(std::fs::File),
    Dir(Option<Vec<File>>),
}

pub struct Sftp {
    root: PathBuf,
    handles: HashMap<String, Open>,
    next: u64,
}

impl Sftp {
    pub fn new(root: PathBuf) -> Self {
        Self { root, handles: HashMap::new(), next: 1 }
    }

    fn real(&self, p: &str) -> PathBuf {
        let rel = p.trim_start_matches('/');
        if rel.is_empty() || rel == "." { self.root.clone() } else { self.root.join(rel) }
    }

    fn handle(&mut self, o: Open) -> String {
        let h = format!("h{}", self.next);
        self.next += 1;
        self.handles.insert(h.clone(), o);
        h
    }
}

fn attrs(m: &std::fs::Metadata) -> FileAttributes {
    let kind = if m.is_dir() {
        0o040000
    } else if m.file_type().is_symlink() {
        0o120000
    } else {
        0o100000
    };
    FileAttributes {
        size: Some(m.len()),
        permissions: Some(kind | if m.is_dir() { 0o755 } else { 0o644 }),
        mtime: m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as u32),
        ..Default::default()
    }
}

fn code(e: std::io::Error) -> StatusCode {
    match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

fn ok(id: u32) -> Status {
    Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() }
}

fn list(dir: &Path) -> std::io::Result<Vec<File>> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let m = e.metadata()?;
        out.push(File::new(e.file_name().to_string_lossy().into_owned(), attrs(&m)));
    }
    Ok(out)
}

impl russh_sftp::server::Handler for Sftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(&mut self, _v: u32, _ext: HashMap<String, String>) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let p = if path.is_empty() || path == "." { "/".to_string() } else { path };
        Ok(Name { id, files: vec![File::dummy(p)] })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let m = std::fs::metadata(self.real(&path)).map_err(code)?;
        Ok(Attrs { id, attrs: attrs(&m) })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let m = std::fs::symlink_metadata(self.real(&path)).map_err(code)?;
        Ok(Attrs { id, attrs: attrs(&m) })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        match self.handles.get(&handle) {
            Some(Open::File(f)) => Ok(Attrs { id, attrs: attrs(&f.metadata().map_err(code)?) }),
            _ => Err(StatusCode::Failure),
        }
    }

    async fn setstat(&mut self, id: u32, _p: String, _a: FileAttributes) -> Result<Status, Self::Error> {
        Ok(ok(id))
    }

    async fn fsetstat(&mut self, id: u32, _h: String, _a: FileAttributes) -> Result<Status, Self::Error> {
        Ok(ok(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let files = list(&self.real(&path)).map_err(code)?;
        Ok(Handle { id, handle: self.handle(Open::Dir(Some(files))) })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        match self.handles.get_mut(&handle) {
            Some(Open::Dir(files)) => match files.take() {
                Some(files) if !files.is_empty() => Ok(Name { id, files }),
                _ => Err(StatusCode::Eof),
            },
            _ => Err(StatusCode::Failure),
        }
    }

    async fn open(
        &mut self,
        id: u32,
        path: String,
        flags: OpenFlags,
        _a: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let f = std::fs::OpenOptions::new()
            .read(flags.contains(OpenFlags::READ))
            .write(flags.contains(OpenFlags::WRITE) || flags.contains(OpenFlags::APPEND))
            .append(flags.contains(OpenFlags::APPEND))
            .create(flags.contains(OpenFlags::CREATE))
            .truncate(flags.contains(OpenFlags::TRUNCATE))
            .open(self.real(&path))
            .map_err(code)?;
        Ok(Handle { id, handle: self.handle(Open::File(f)) })
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, Self::Error> {
        let Some(Open::File(f)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        f.seek(SeekFrom::Start(offset)).map_err(code)?;
        let mut buf = vec![0; len.min(256 * 1024) as usize];
        let n = f.read(&mut buf).map_err(code)?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, Self::Error> {
        let Some(Open::File(f)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        f.seek(SeekFrom::Start(offset)).map_err(code)?;
        f.write_all(&data).map_err(code)?;
        Ok(ok(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(ok(id))
    }

    async fn mkdir(&mut self, id: u32, path: String, _a: FileAttributes) -> Result<Status, Self::Error> {
        std::fs::create_dir(self.real(&path)).map_err(code)?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        std::fs::remove_dir(self.real(&path)).map_err(code)?;
        Ok(ok(id))
    }

    async fn remove(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        std::fs::remove_file(self.real(&path)).map_err(code)?;
        Ok(ok(id))
    }

    async fn rename(&mut self, id: u32, from: String, to: String) -> Result<Status, Self::Error> {
        std::fs::rename(self.real(&from), self.real(&to)).map_err(code)?;
        Ok(ok(id))
    }
}
