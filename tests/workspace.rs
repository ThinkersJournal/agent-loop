// SPDX-License-Identifier: MIT OR Apache-2.0
//! Workspace mode selection: B2 by default, B1 only for a policy file a person approved for that root, fail closed.
use agent_loop::{
    MemoryApprovals, PolicyError, Sandbox, SandboxError, Selection, Why, Workspace, WorkspaceMode,
    open_workspace, policy_hash, requested_mode, select_mode,
};

const ROOT: &str = r"C:\Projects\demo";
const BIND: &str = "[workspace]\nmode = \"bind\"\n";

fn approvals_for(text: &str, root: &str) -> MemoryApprovals {
    let mut a = MemoryApprovals::new();
    a.approve(root, WorkspaceMode::Bind, &policy_hash(text));
    a
}

#[test]
fn default_is_copy_back() {
    let none = MemoryApprovals::new();
    assert_eq!(
        select_mode(None, ROOT, &none),
        Selection {
            mode: WorkspaceMode::CopyBack,
            why: Why::Default
        }
    );
    // A policy file that says nothing about the workspace is the default too.
    let s = select_mode(Some("[command]\nanything = 1\n"), ROOT, &none);
    assert_eq!((s.mode, s.why), (WorkspaceMode::CopyBack, Why::Default));
    // Asking for the safe mode needs no approval.
    let s = select_mode(Some("[workspace]\nmode = \"copy-back\"\n"), ROOT, &none);
    assert_eq!((s.mode, s.why), (WorkspaceMode::CopyBack, Why::Policy));
}

#[test]
fn bind_needs_an_approval_of_this_file_for_this_root() {
    // Positive control: with the approval, the same input selects Bind.
    let approved = approvals_for(BIND, ROOT);
    let s = select_mode(Some(BIND), ROOT, &approved);
    assert_eq!((s.mode, s.why), (WorkspaceMode::Bind, Why::Approved));

    // No approval at all.
    let s = select_mode(Some(BIND), ROOT, &MemoryApprovals::new());
    assert_eq!((s.mode, s.why), (WorkspaceMode::CopyBack, Why::Unapproved));

    // A cloned repo's file is a different file: an approval of other text does not carry over.
    let edited = "[workspace]\nmode = \"bind\"\n# edited\n";
    let s = select_mode(Some(edited), ROOT, &approved);
    assert_eq!((s.mode, s.why), (WorkspaceMode::CopyBack, Why::Unapproved));

    // The same approved text for another root is not approved.
    let s = select_mode(Some(BIND), r"C:\Projects\other", &approved);
    assert_eq!((s.mode, s.why), (WorkspaceMode::CopyBack, Why::Unapproved));
}

#[test]
fn malformed_policy_falls_back_to_copy_back_even_if_approved() {
    let bad = [
        "[workspace]\nmode = \"auto\"\n",
        "[workspace]\nmode = \"bind\"\nextra = true\n",
        "[workspace\nmode = ",
        "[workspace]\nmode = 3\n",
    ];
    for text in bad {
        let approved = approvals_for(text, ROOT);
        let s = select_mode(Some(text), ROOT, &approved);
        assert_eq!(s.mode, WorkspaceMode::CopyBack, "{text:?}");
        assert!(matches!(s.why, Why::Malformed(_)), "{text:?}: {:?}", s.why);
    }
    assert!(matches!(
        requested_mode("[workspace]\nmode = \"auto\"\n"),
        Err(PolicyError::UnknownMode(_))
    ));
}

#[test]
fn policy_hash_is_the_sha256_of_the_exact_bytes() {
    // SHA-256 of the empty string, a published test vector.
    assert_eq!(
        policy_hash(""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_ne!(policy_hash(BIND), policy_hash(&format!("{BIND}\n")));
}

/// A sandbox that can give only one mode and records every attempt.
struct OneMode {
    can: WorkspaceMode,
    attempts: Vec<WorkspaceMode>,
}
impl Sandbox for OneMode {
    fn open_session(&mut self, _id: &str, w: &Workspace) -> Result<(), SandboxError> {
        self.attempts.push(w.mode);
        if w.mode == self.can {
            Ok(())
        } else {
            Err(SandboxError::Unmet(format!("workspace mode {}", w.mode)))
        }
    }
}

#[test]
fn a_refused_mode_is_never_retried_in_the_other_one() {
    // The sandbox can only do bind; the selection is copy-back (the default). It must refuse, not fall through to bind.
    let mut sb = OneMode {
        can: WorkspaceMode::Bind,
        attempts: vec![],
    };
    let sel = select_mode(None, ROOT, &MemoryApprovals::new());
    let err = open_workspace(&mut sb, "s1", ROOT, &sel).unwrap_err();
    assert_eq!(err, SandboxError::Unmet("workspace mode copy-back".into()));
    assert_eq!(sb.attempts, [WorkspaceMode::CopyBack]);

    // Control: a sandbox that can do copy-back opens it, and the workspace records the mode.
    let mut ok = OneMode {
        can: WorkspaceMode::CopyBack,
        attempts: vec![],
    };
    let w = open_workspace(&mut ok, "s1", ROOT, &sel).unwrap();
    assert_eq!((w.root.as_str(), w.mode), (ROOT, WorkspaceMode::CopyBack));
}

#[test]
fn approved_bind_opens_bind() {
    let approved = approvals_for(BIND, ROOT);
    let sel = select_mode(Some(BIND), ROOT, &approved);
    let mut sb = OneMode {
        can: WorkspaceMode::Bind,
        attempts: vec![],
    };
    let w = open_workspace(&mut sb, "s1", ROOT, &sel).unwrap();
    assert_eq!(w.mode, WorkspaceMode::Bind);
    assert_eq!(sb.attempts, [WorkspaceMode::Bind]);
}
