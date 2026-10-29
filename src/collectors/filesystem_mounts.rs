use std::path::{Path, PathBuf};

use crate::{collector::CollectorError, config::MountRequirement};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MountMetadata {
    pub(super) mount_point: PathBuf,
    pub(super) source: String,
    pub(super) filesystem_type: String,
    pub(super) read_only: bool,
    pub(super) agent_view_read_only: bool,
}

pub(super) async fn inspect(
    path: &Path,
    requirement: MountRequirement,
) -> Result<Option<MountMetadata>, CollectorError> {
    let canonical_path = tokio::fs::canonicalize(path)
        .await
        .map_err(|error| CollectorError::new("filesystem_path", error.to_string()))?;
    inspect_platform(&canonical_path, requirement).await
}

#[cfg(target_os = "linux")]
async fn inspect_platform(
    path: &Path,
    requirement: MountRequirement,
) -> Result<Option<MountMetadata>, CollectorError> {
    let contents = tokio::fs::read_to_string("/proc/self/mountinfo")
        .await
        .map_err(|error| CollectorError::new("mountinfo", error.to_string()))?;
    let mounts = parse_mountinfo(&contents);
    let metadata = match requirement {
        MountRequirement::Path => containing_mount(path, mounts.iter()).cloned(),
        MountRequirement::Mountpoint => mounts.into_iter().find(|mount| mount.mount_point == path),
    };
    if requirement == MountRequirement::Mountpoint && metadata.is_none() {
        return Err(CollectorError::new(
            "mount_missing",
            format!("expected mount point {} is not mounted", path.display()),
        ));
    }
    Ok(metadata)
}

#[cfg(not(target_os = "linux"))]
async fn inspect_platform(
    _path: &Path,
    requirement: MountRequirement,
) -> Result<Option<MountMetadata>, CollectorError> {
    match requirement {
        MountRequirement::Path => Ok(None),
        MountRequirement::Mountpoint => Err(CollectorError::new(
            "mount_check_unsupported",
            "exact mount-point checks are supported only on Linux",
        )),
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_mountinfo(contents: &str) -> Vec<MountMetadata> {
    contents.lines().filter_map(parse_mount).collect()
}

#[cfg(any(target_os = "linux", test))]
fn parse_mount(line: &str) -> Option<MountMetadata> {
    let (left, right) = line.split_once(" - ")?;
    let mut left_fields = left.split_whitespace();
    let _mount_id = left_fields.next()?;
    let _parent_id = left_fields.next()?;
    let _device = left_fields.next()?;
    let _root = left_fields.next()?;
    let mount_point = decode_mount_field(left_fields.next()?);
    let options = left_fields.next()?;
    let mut right_fields = right.split_whitespace();
    let filesystem_type = decode_mount_field(right_fields.next()?);
    let source = decode_mount_field(right_fields.next()?);
    let super_options = right_fields.next()?;
    Some(MountMetadata {
        mount_point: PathBuf::from(mount_point),
        source,
        filesystem_type,
        read_only: option_is_read_only(super_options),
        agent_view_read_only: option_is_read_only(options),
    })
}

#[cfg(any(target_os = "linux", test))]
fn option_is_read_only(options: &str) -> bool {
    options.split(',').any(|option| option == "ro")
}

#[cfg(any(target_os = "linux", test))]
fn decode_mount_field(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

#[cfg(any(target_os = "linux", test))]
fn containing_mount<'a>(
    path: &Path,
    mounts: impl Iterator<Item = &'a MountMetadata>,
) -> Option<&'a MountMetadata> {
    mounts
        .filter(|mount| path.starts_with(&mount.mount_point))
        .max_by_key(|mount| mount.mount_point.components().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parses_mount_metadata_and_escaped_path() {
        // Arrange
        let input = "36 25 8:1 / /srv/data\\040disk ro,relatime - ext4 /dev/sdb1 rw";

        // Act
        let mounts = parse_mountinfo(input);

        // Assert
        assert_eq!(
            mounts,
            vec![MountMetadata {
                mount_point: PathBuf::from("/srv/data disk"),
                source: "/dev/sdb1".to_owned(),
                filesystem_type: "ext4".to_owned(),
                read_only: false,
                agent_view_read_only: true,
            }]
        );
    }

    #[test]
    fn test_selects_most_specific_containing_mount() {
        // Arrange
        let root = a_mount("/");
        let data = a_mount("/srv/data");
        let mounts = [root, data];

        // Act
        let selected = containing_mount(Path::new("/srv/data/files"), mounts.iter());

        // Assert
        assert_eq!(
            selected.map(|mount| mount.mount_point.as_path()),
            Some(Path::new("/srv/data"))
        );
    }

    fn a_mount(path: &str) -> MountMetadata {
        MountMetadata {
            mount_point: PathBuf::from(path),
            source: "device".to_owned(),
            filesystem_type: "ext4".to_owned(),
            read_only: false,
            agent_view_read_only: false,
        }
    }
}
