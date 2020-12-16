use nix::unistd::{getegid, geteuid, getgid, getuid, setegid, seteuid, Gid, Uid};
use std::error::Error;

#[derive(Debug, Clone)]
pub struct ExecutionContext {
    pub is_suid: bool,
    pub is_sgid: bool,
    pub uid: Uid,
    pub euid: Uid,
    pub gid: Gid,
    pub egid: Gid,
}

impl Default for ExecutionContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionContext {
    pub fn new() -> Self {
        Self {
            is_suid: getuid() != geteuid(),
            is_sgid: getgid() != getegid(),
            uid: getuid(),
            euid: geteuid(),
            gid: getgid(),
            egid: getegid(),
        }
    }

    pub fn drop_privileges(&self) -> Result<(), nix::Error> {
        debug!("Dropping privileges -> {}/{}", self.uid, self.gid);
        if self.is_suid {
            seteuid(self.uid)?;
        }
        if self.is_sgid {
            setegid(self.gid)?;
        }
        Ok(())
    }

    pub fn regain_privileges(&self) -> Result<(), nix::Error> {
        debug!("Regaining privileges -> {}/{}", self.euid, self.egid);
        if self.is_suid {
            seteuid(self.euid)?;
        }
        if self.is_sgid {
            setegid(self.egid)?;
        }
        Ok(())
    }

    /// Convenience helper which brackets a closure with drop/regain privileges
    pub fn with_dropped_privileges<T, E, F>(&self, unprivileged: F) -> Result<T, E>
    where
        E: Error + From<nix::Error>,
        F: FnOnce() -> Result<T, E>,
    {
        self.drop_privileges()?;
        let res = unprivileged();
        self.regain_privileges()?;
        res
    }
}
