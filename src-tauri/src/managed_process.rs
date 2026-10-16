//! An inference job owns its process group, including nested native workers.
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

pub fn isolate(command: &mut Command) {
    if std::env::var_os("MURMELA_INFERENCE_GROUP").is_none() {
        command.process_group(0);
    }
    command.env("MURMELA_INFERENCE_GROUP", "1");
}

fn terminate(child: &mut Child, owns_group: bool) {
    unsafe {
        // Only target the group created for this child, never our own group.
        if owns_group {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub struct ManagedChild(pub Child, bool);
impl ManagedChild {
    pub fn new(child: Child) -> Self {
        Self(child, std::env::var_os("MURMELA_INFERENCE_GROUP").is_none())
    }
    pub fn kill(&mut self) -> std::io::Result<()> {
        terminate(&mut self.0, self.1);
        Ok(())
    }
}
impl std::ops::Deref for ManagedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}
impl std::ops::DerefMut for ManagedChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}
impl Drop for ManagedChild {
    fn drop(&mut self) {
        terminate(&mut self.0, self.1);
    }
}
