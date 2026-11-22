use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use super::CameraDeviceInfo;

pub fn list_camera_devices() -> Vec<CameraDeviceInfo> {
    scan(Path::new("/sys/class/video4linux"), Path::new("/dev"))
}

fn video_number(node: &str) -> Option<u32> {
    let number = node.strip_prefix("video")?;
    if !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}

pub(crate) fn is_numbered_camera(id: &str) -> bool {
    id.strip_prefix("/dev/").and_then(video_number).is_some()
}

fn scan(video_class: &Path, dev: &Path) -> Vec<CameraDeviceInfo> {
    let Ok(entries) = fs::read_dir(video_class) else {
        return Vec::new();
    };
    let ports = port_paths(&dev.join("v4l/by-path"));
    let mut cameras: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let node = entry.file_name();
            let node = node.to_str()?;
            let number = video_number(node)?;
            let numbered = dev.join(node);
            let target = numbered.canonicalize().ok()?;
            let id = ports.get(&target).unwrap_or(&numbered);
            let name = fs::read_to_string(entry.path().join("name"))
                .ok()
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| node.to_owned());
            Some((
                number,
                CameraDeviceInfo {
                    id: id.to_str()?.to_owned(),
                    name,
                    numbered_id: (id != &numbered).then(|| numbered.to_string_lossy().into_owned()),
                },
            ))
        })
        .collect();
    cameras.sort_unstable_by_key(|(number, _)| *number);
    cameras.into_iter().map(|(_, camera)| camera).collect()
}

fn port_paths(port_dir: &Path) -> HashMap<PathBuf, PathBuf> {
    let Ok(entries) = fs::read_dir(port_dir) else {
        return HashMap::new();
    };
    let mut links: Vec<_> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_symlink()))
        .map(|entry| entry.path())
        .collect();
    links.sort_unstable();
    let mut ports = HashMap::new();
    for link in links {
        if let Ok(target) = link.canonicalize() {
            ports.entry(target).or_insert(link);
        }
    }
    ports
}

/// Verify that the exact saved Linux route still resolves to a V4L2 node.
/// `stat` follows udev links without opening or reserving the capture device.
pub(crate) fn check_camera_device(id: &str) -> Result<(), String> {
    const VIDEO4LINUX_MAJOR: u32 = 81;
    match fs::metadata(id) {
        Ok(metadata)
            if metadata.file_type().is_char_device()
                && libc::major(metadata.rdev()) == VIDEO4LINUX_MAJOR =>
        {
            Ok(())
        }
        Ok(_) => Err(format!(
            "Camera input {id} is not a V4L2 device. Select a camera in the Camera inspector."
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Err(format!(
            "Camera input {id} is missing. Reconnect it or choose a device in the Camera inspector."
        )),
        Err(error) => Err(format!("Could not check camera input {id}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn add_camera(class: &Path, dev: &Path, node: &str) {
        fs::create_dir(class.join(node)).unwrap();
        fs::write(class.join(node).join("name"), "USB capture\n").unwrap();
        fs::File::create(dev.join(node)).unwrap();
    }

    #[test]
    fn capture_binding_follows_a_port_when_linux_renumbers_its_node() {
        let fixture = tempfile::tempdir().unwrap();
        let class = fixture.path().join("class");
        let dev = fixture.path().join("dev");
        fs::create_dir(&class).unwrap();
        fs::create_dir_all(dev.join("v4l/by-path")).unwrap();
        add_camera(&class, &dev, "video5");
        add_camera(&class, &dev, "video8");
        let left = dev.join("v4l/by-path/usb-1-video-index0");
        let right = dev.join("v4l/by-path/usb-2-video-index0");
        symlink("../../video5", &left).unwrap();
        symlink("../../video8", &right).unwrap();
        let initial = scan(&class, &dev);
        assert_eq!(initial.len(), 2);
        assert_eq!(initial[0].id, left.to_str().unwrap());
        assert_eq!(
            initial[0].numbered_id.as_deref(),
            dev.join("video5").to_str()
        );
        assert_eq!(initial[1].id, right.to_str().unwrap());
        assert_eq!(initial[0].name, initial[1].name);

        fs::remove_dir_all(class.join("video5")).unwrap();
        fs::remove_file(dev.join("video5")).unwrap();
        assert_eq!(scan(&class, &dev)[0].id, initial[1].id);
        assert!(check_camera_device(&initial[0].id)
            .unwrap_err()
            .contains("is missing"));

        add_camera(&class, &dev, "video12");
        fs::remove_file(&left).unwrap();
        symlink("../../video12", &left).unwrap();
        let current = scan(&class, &dev);
        assert_eq!(current[1].id, initial[0].id);
        assert_eq!(
            current[1].numbered_id.as_deref(),
            dev.join("video12").to_str()
        );
        assert_eq!(current[0].id, initial[1].id);
    }

    #[test]
    fn a_device_without_a_port_link_remains_selectable_by_its_number() {
        let fixture = tempfile::tempdir().unwrap();
        let class = fixture.path().join("class");
        let dev = fixture.path().join("dev");
        fs::create_dir(&class).unwrap();
        fs::create_dir(&dev).unwrap();
        add_camera(&class, &dev, "video10");
        add_camera(&class, &dev, "video2");
        add_camera(&class, &dev, "video-bogus");
        let choices = scan(&class, &dev);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].id, dev.join("video2").to_str().unwrap());
        assert!(choices.iter().all(|device| device.numbered_id.is_none()));
        assert!(is_numbered_camera("/dev/video2"));
        assert!(!is_numbered_camera("/dev/v4l/by-path/camera"));
        assert!(check_camera_device("/dev/null")
            .unwrap_err()
            .contains("is not a V4L2 device"));
    }
}
