use std::{
    fs::{self, File, Metadata},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result};

use crate::index::{ArtifactCheckpoint, ArtifactRecord};

const BOUNDARY_BYTES: i64 = 4 * 1_024;

#[derive(Clone, Debug)]
pub struct FileMetadata {
    pub device: Option<i64>,
    pub inode: Option<i64>,
    pub size: i64,
    pub modified_ns: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanPlan {
    Unchanged,
    Append(i64),
    Full,
}

#[derive(Clone, Debug)]
pub struct LineScan {
    pub parsed_offset: i64,
    pub full_hash: Option<Vec<u8>>,
}

pub fn discover(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for root in roots {
        collect(root, &mut paths)?;
    }
    paths.sort();
    Ok(paths)
}

fn collect(path: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            paths.push(path.to_path_buf());
        }
        return Ok(());
    }
    if !metadata.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(path).with_context(|| format!("listing {}", path.display()))? {
        collect(&entry?.path(), paths)?;
    }
    Ok(())
}

pub fn artifact_key(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

pub fn metadata(path: &Path) -> Result<FileMetadata> {
    let metadata = fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    let (device, inode) = file_identity(&metadata);
    Ok(FileMetadata {
        device,
        inode,
        size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
        modified_ns: modified_ns(&metadata),
    })
}

pub fn plan(
    path: &Path,
    metadata: &FileMetadata,
    checkpoint: Option<&ArtifactCheckpoint>,
    parser_version: i64,
    force_full: bool,
) -> Result<ScanPlan> {
    if force_full {
        return Ok(ScanPlan::Full);
    }
    let Some(checkpoint) = checkpoint else {
        return Ok(ScanPlan::Full);
    };
    if checkpoint.parser_version != parser_version {
        return Ok(ScanPlan::Full);
    }
    if checkpoint.size == Some(metadata.size) && checkpoint.modified_ns == metadata.modified_ns {
        return Ok(ScanPlan::Unchanged);
    }
    let same_file = checkpoint
        .device
        .zip(metadata.device)
        .is_none_or(|(old, new)| old == new)
        && checkpoint
            .inode
            .zip(metadata.inode)
            .is_none_or(|(old, new)| old == new);
    if !same_file || metadata.size < checkpoint.parsed_offset {
        return Ok(ScanPlan::Full);
    }
    if metadata.size <= checkpoint.size.unwrap_or(checkpoint.parsed_offset) {
        return Ok(ScanPlan::Full);
    }
    let boundary_matches = match &checkpoint.boundary_hash {
        Some(expected) => boundary_hash(path, checkpoint.parsed_offset)? == *expected,
        None => checkpoint.parsed_offset == 0,
    };
    if boundary_matches {
        Ok(ScanPlan::Append(checkpoint.parsed_offset))
    } else {
        Ok(ScanPlan::Full)
    }
}

pub fn scan_lines(
    path: &Path,
    start_offset: i64,
    hash_full_file: bool,
    mut visit: impl FnMut(&[u8]),
) -> Result<LineScan> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    file.seek(SeekFrom::Start(start_offset.max(0) as u64))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut parsed_offset = start_offset.max(0);
    let mut hasher = hash_full_file.then(blake3::Hasher::new);

    loop {
        line.clear();
        let bytes = reader.read_until(b'\n', &mut line)?;
        if bytes == 0 {
            break;
        }
        if let Some(hasher) = &mut hasher {
            hasher.update(&line);
        }
        if !line.ends_with(b"\n") {
            break;
        }
        visit(&line);
        parsed_offset = parsed_offset.saturating_add(i64::try_from(bytes).unwrap_or(i64::MAX));
    }

    Ok(LineScan {
        parsed_offset,
        full_hash: hasher.map(|hasher| hasher.finalize().as_bytes().to_vec()),
    })
}

pub fn first_line(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line)?;
    Ok(line)
}

pub fn boundary_hash(path: &Path, parsed_offset: i64) -> Result<Vec<u8>> {
    let end = parsed_offset.max(0);
    let start = end.saturating_sub(BOUNDARY_BYTES).max(0);
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    file.seek(SeekFrom::Start(start as u64))?;
    let length = usize::try_from(end.saturating_sub(start)).unwrap_or(0);
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes)?;
    Ok(blake3::hash(&bytes).as_bytes().to_vec())
}

pub fn artifact(
    key: String,
    path: &Path,
    metadata: &FileMetadata,
    scan: &LineScan,
    cursor: Option<String>,
    parser_version: i64,
    scanned_at_ms: i64,
) -> Result<ArtifactRecord> {
    Ok(ArtifactRecord {
        key,
        path: Some(path.display().to_string()),
        device: metadata.device,
        inode: metadata.inode,
        size: Some(metadata.size),
        modified_ns: metadata.modified_ns,
        parsed_offset: scan.parsed_offset,
        boundary_hash: Some(boundary_hash(path, scan.parsed_offset)?),
        full_hash: scan.full_hash.clone(),
        cursor,
        parser_version,
        scanned_at_ms,
    })
}

fn modified_ns(metadata: &Metadata) -> Option<i64> {
    let duration = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_nanos()).ok()
}

#[cfg(unix)]
fn file_identity(metadata: &Metadata) -> (Option<i64>, Option<i64>) {
    use std::os::unix::fs::MetadataExt;
    (
        i64::try_from(metadata.dev()).ok(),
        i64::try_from(metadata.ino()).ok(),
    )
}

#[cfg(not(unix))]
fn file_identity(_metadata: &Metadata) -> (Option<i64>, Option<i64>) {
    (None, None)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn resumes_only_after_complete_lines() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"{\"one\":1}\n{\"partial\":").unwrap();
        file.flush().unwrap();
        let mut lines = Vec::new();

        let scan = scan_lines(file.path(), 0, true, |line| lines.push(line.to_vec())).unwrap();

        assert_eq!(lines, vec![b"{\"one\":1}\n".to_vec()]);
        assert_eq!(scan.parsed_offset, 10);
        assert!(scan.full_hash.is_some());
    }

    #[test]
    fn detects_safe_append_and_in_place_rewrite() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"first\n").unwrap();
        file.flush().unwrap();
        let initial_metadata = metadata(file.path()).unwrap();
        let checkpoint = ArtifactCheckpoint {
            device: initial_metadata.device,
            inode: initial_metadata.inode,
            size: Some(initial_metadata.size),
            modified_ns: initial_metadata.modified_ns,
            parsed_offset: initial_metadata.size,
            boundary_hash: Some(boundary_hash(file.path(), initial_metadata.size).unwrap()),
            full_hash: None,
            cursor: None,
            parser_version: 1,
        };

        file.write_all(b"second\n").unwrap();
        file.flush().unwrap();
        let appended = metadata(file.path()).unwrap();
        assert_eq!(
            plan(file.path(), &appended, Some(&checkpoint), 1, false).unwrap(),
            ScanPlan::Append(6)
        );

        std::fs::write(file.path(), b"other!\nmore\n").unwrap();
        let rewritten = metadata(file.path()).unwrap();
        assert_eq!(
            plan(file.path(), &rewritten, Some(&checkpoint), 1, false).unwrap(),
            ScanPlan::Full
        );
    }
}
