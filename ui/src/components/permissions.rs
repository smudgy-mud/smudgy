//! The package permission-consent rendering: risk tiers, the "can do" line
//! enumeration, and the "effectively full access" banner.
//!
//! Extracted from the Automations window's package panes so every surface that asks the
//! user to judge a permission union — the install/update consent cards there and the main
//! window's update-review modal — renders the *same* tiers, rows, and banner. The view
//! builders are generic over the window's message type; they emit no messages of their
//! own.

use std::collections::HashSet;

use iced::Length;
use iced::alignment::Vertical;
use iced::widget::{column, container, row, text};

use smudgy_core::models::shared_packages::{
    ImportPolicy, IpcEntry, PackagePermissions, SmudgyCapabilities, is_any_host_net_entry,
};

use crate::assets::fonts;
use crate::theme::Element;
use crate::windows::automations_window::common;

/// How dangerous one granted permission is — the tier that drives the consent/pane styling, so
/// the display conveys *risk*, not just a flat list (the deno-style framing: some grants are
/// scoped capabilities, some are the whole computer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PermissionRisk {
    /// A scoped grant that does what the line says and nothing more.
    Normal,
    /// Elevated exposure (reading files outside the package's own data folder, connecting to any
    /// host, downloading arbitrary web code to run) — flagged amber, but still contained by the
    /// sandbox.
    Caution,
    /// Sandbox-escape-equivalent: subprocesses (`run`), native code (`ffi`), or writes outside
    /// `$DATA`. A subprocess or native library runs with the user's full privileges, and an
    /// outside write can rewrite config/scripts/other packages — whatever the line says, the
    /// honest summary is "effectively full access".
    Critical,
}

/// A single "can do" / "cannot do" line in the consent enumeration.
pub(crate) struct PermissionLine {
    /// The capability label (e.g. `"connect to"`, `"read"`), or the categorical denial.
    pub(crate) head: String,
    /// The specific target (host/path/var/program), when the line lists one.
    pub(crate) detail: Option<String>,
    /// How this line should be framed (colors + the full-access banner roll-up).
    pub(crate) risk: PermissionRisk,
}

/// The `import` "can do" line for the consent enumeration — one line whose wording follows the
/// tri-state [`ImportPolicy`]. `None` shows nothing (the "cannot" list covers it).
fn import_can_line(policy: ImportPolicy) -> Option<&'static str> {
    match policy {
        ImportPolicy::None => None,
        ImportPolicy::Registries => Some(crate::i18n::ts!("permission-import-registries")),
        ImportPolicy::Any => Some(crate::i18n::ts!("permission-import-anywhere")),
    }
}

/// The "this package will be able to" lines for a granted union: one per host/path/var/program.
/// Empty when the package asks for nothing — callers phrase the no-access case in context (see
/// `sandbox_summary`). Lines carry a [`PermissionRisk`] so the rows and the callers' full-access
/// banner ([`union_risk`]) agree on what's scoped and what's effectively unlimited.
pub(crate) fn permission_can_lines(perms: &PackagePermissions) -> Vec<PermissionLine> {
    let mut lines = Vec::new();
    // Hosts dedup case-insensitively (DNS is case-insensitive) so the list shows no near-dupes,
    // keeping each host's first-seen spelling.
    let mut seen_hosts: HashSet<String> = HashSet::new();
    for host in &perms.net {
        if seen_hosts.insert(host.trim().to_lowercase()) {
            lines.push(PermissionLine {
                head: crate::i18n::t!("permission-connect-to"),
                detail: Some(host.clone()),
                // `*` / `*:port` lets the package choose the peer rather than constraining it to
                // a named host. Frame that at the same caution tier as arbitrary web-code imports.
                risk: if is_any_host_net_entry(host) {
                    PermissionRisk::Caution
                } else {
                    PermissionRisk::Normal
                },
            });
        }
    }
    // Local IPC rows: one line per endpoint showing BOTH realizations distinctly, the one that
    // does not apply to this computer annotated (it is inert on this install). A local socket
    // can front a privileged daemon (for example Docker), so every row sits at the same
    // full-access cliff as run/ffi.
    let mut seen_ipc: HashSet<String> = HashSet::new();
    for row in &perms.ipc {
        let detail = ipc_line_detail(row);
        if seen_ipc.insert(detail.clone()) {
            lines.push(PermissionLine {
                head: crate::i18n::t!("permission-connect-local-ipc"),
                detail: Some(detail),
                risk: PermissionRisk::Critical,
            });
        }
    }
    // `import` is a separate axis from `net`: it downloads third-party code to RUN (sandboxed, but
    // not visible in the package source you reviewed), rather than opening a data connection.
    // Registry code is at least published/auditable; "anywhere on the web" is not — amber.
    if let Some(head) = import_can_line(perms.import) {
        lines.push(PermissionLine {
            head: head.to_string(),
            detail: None,
            risk: if perms.import == ImportPolicy::Any {
                PermissionRisk::Caution
            } else {
                PermissionRisk::Normal
            },
        });
    }
    // Only advertise read/write/ffi paths the engine will actually grant: a `$DATA/..` entry is
    // dropped by the enforcement guardrail (it would escape the data dir), so it isn't a real
    // capability. A path OUTSIDE the package's own data folder changes the line's meaning: a read
    // reaches the user's files (privacy), a write can rewrite config/scripts/other packages — the
    // unbox — so it is flagged, not listed as if it were a scoped grant.
    for path in perms.read.iter().filter(|p| path_grant_enforced(p)) {
        let scoped = data_scoped(path);
        lines.push(PermissionLine {
            head: if scoped {
                "read"
            } else {
                "read (outside its data folder)"
            }
            .to_string(),
            detail: Some(pretty_path(path)),
            risk: if scoped {
                PermissionRisk::Normal
            } else {
                PermissionRisk::Caution
            },
        });
    }
    for path in perms.write.iter().filter(|p| path_grant_enforced(p)) {
        let scoped = data_scoped(path);
        lines.push(PermissionLine {
            head: if scoped {
                "write"
            } else {
                "write (outside its data folder)"
            }
            .to_string(),
            detail: Some(pretty_path(path)),
            risk: if scoped {
                PermissionRisk::Normal
            } else {
                PermissionRisk::Critical
            },
        });
    }
    for var in &perms.env {
        lines.push(PermissionLine {
            head: crate::i18n::t!("permission-read-env"),
            detail: Some(var.clone()),
            risk: PermissionRisk::Normal,
        });
    }
    // Subprocesses and native libraries run OUTSIDE the sandbox with your full privileges —
    // always critical, however narrow the listed target looks.
    for program in &perms.run {
        lines.push(PermissionLine {
            head: "run the program".to_string(),
            detail: Some(program.clone()),
            risk: PermissionRisk::Critical,
        });
    }
    for path in perms.ffi.iter().filter(|p| path_grant_enforced(p)) {
        lines.push(PermissionLine {
            head: "load the native library".to_string(),
            detail: Some(pretty_path(path)),
            risk: PermissionRisk::Critical,
        });
    }
    // System details roll into one line (fingerprinting-grade info, not a capability).
    if !perms.sys.is_empty() {
        lines.push(PermissionLine {
            head: "read system details".to_string(),
            detail: Some(perms.sys.join(", ")),
            risk: PermissionRisk::Normal,
        });
    }
    // The granted smudgy op-capabilities (one row each, no target list).
    lines.extend(smudgy_can_lines(&perms.smudgy));
    lines
}

/// The consent-line detail for one `ipc` row: both realizations, each labeled by kind (Unix
/// socket path vs Windows pipe name), with the realization that does not match the running
/// platform annotated — it contributes nothing to the grant on this install, and honest consent
/// says so.
fn ipc_line_detail(row: &IpcEntry) -> String {
    let foreign = crate::i18n::t!("permission-ipc-foreign-platform");
    let mut parts = Vec::new();
    if let Some(path) = &row.unix {
        let part = crate::i18n::t!("permission-ipc-unix-socket", "path" => path.as_str());
        parts.push(if cfg!(windows) {
            format!("{part} ({foreign})")
        } else {
            part
        });
    }
    if let Some(name) = &row.windows_pipe {
        let part = crate::i18n::t!("permission-ipc-windows-pipe", "name" => name.as_str());
        parts.push(if cfg!(windows) {
            part
        } else {
            format!("{part} ({foreign})")
        });
    }
    parts.join(" \u{00B7} ")
}

/// A smudgy op-capability "can do" line with no target list (the head text is the whole label).
fn cap_line(head: &str) -> PermissionLine {
    PermissionLine {
        head: head.to_string(),
        detail: None,
        risk: PermissionRisk::Normal,
    }
}

/// Whether a `read`/`write`/`ffi` entry stays inside the package's OWN data folder (the `$DATA`
/// placeholder). Callers filter `..`-escapes with [`path_grant_enforced`] first, so a `$DATA/…`
/// entry seen here really is contained; a `$DATA`-lookalike (`$DATABASE`) or an absolute path is
/// not. Outside-`$DATA` grants are what change a file permission from "its own storage" to "your
/// computer" — the risk cliff the consent framing keys on. `pub(crate)` so the manifest editor
/// warns the author on the same predicate installers will be warned on.
pub(crate) fn data_scoped(entry: &str) -> bool {
    let Some(rest) = entry.trim().strip_prefix("$DATA") else {
        return false;
    };
    matches!(rest.chars().next(), None | Some('/' | '\\'))
}

/// The highest [`PermissionRisk`] across a union's lines — what decides whether a pane shows the
/// full-access banner over the enumeration.
pub(crate) fn union_risk(perms: &PackagePermissions) -> PermissionRisk {
    permission_can_lines(perms)
        .iter()
        .map(|line| line.risk)
        .max()
        .unwrap_or(PermissionRisk::Normal)
}

/// The specific sandbox-escape grants in a union, phrased for the full-access banner ("it can
/// {a}, {b}"). Empty iff the union has no [`PermissionRisk::Critical`] line.
pub(crate) fn escape_reasons(perms: &PackagePermissions) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if !perms.ipc.is_empty() {
        reasons.push("connect to local IPC services");
    }
    if !perms.run.is_empty() {
        reasons.push("run other programs");
    }
    if perms.ffi.iter().any(|p| path_grant_enforced(p)) {
        reasons.push("load native code");
    }
    if perms
        .write
        .iter()
        .any(|p| path_grant_enforced(p) && !data_scoped(p))
    {
        reasons.push("write files outside its own data folder");
    }
    reasons
}

/// The "effectively full access" banner shown over a permission enumeration whose union contains a
/// sandbox-escape grant ([`escape_reasons`]). One honest paragraph instead of letting a
/// scoped-looking line (`run git`) read like a scoped grant: programs it runs, native code it
/// loads, and files it writes outside its data folder are NOT sandboxed. `None` when the union has
/// no critical grant.
pub(crate) fn full_access_banner<'a, M: 'a>(perms: &PackagePermissions) -> Option<Element<'a, M>> {
    let reasons = escape_reasons(perms);
    if reasons.is_empty() {
        return None;
    }
    Some(
        container(
            column![
                row![
                    text("\u{26A0}").size(14.0).style(common::danger),
                    text(crate::i18n::t!("package-effectively-full-access"))
                        .size(14.0)
                        .style(common::danger),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
                text(format!(
                    "Because this package can {}, those capabilities can affect your computer in \
                    ways the sandbox in smudgy cannot offer protection from. Be certain that you \
                    trust it before enabling it.",
                    join_reasons(&reasons)
                ))
                .size(12.0),
            ]
            .spacing(6.0),
        )
        .padding(12.0)
        .width(Length::Fill)
        .style(common::banner_style)
        .into(),
    )
}

/// Join escape reasons into prose: `a`, `a and b`, `a, b, and c`.
pub(crate) fn join_reasons(reasons: &[&str]) -> String {
    match reasons {
        [one] => (*one).to_string(),
        [a, b] => format!("{a} and {b}"),
        [head @ .., last] => format!("{}, and {last}", head.join(", ")),
        [] => String::new(),
    }
}

/// The smudgy op-capability "can do" rows for the consent window
/// (`PACKAGE-ISOLATES-OP-CAPABILITIES.md`), in a stable grouped order. `send`/`send-direct` are
/// one combined, nuanced row whose wording depends on which (neither/either/both) is granted
/// ([`send_can_line`]); `change_display` describes what it can do (hide/restyle/inject/replace)
/// plainly; `reach-others` is a normal row, not flagged high-risk.
fn smudgy_can_lines(caps: &SmudgyCapabilities) -> Vec<PermissionLine> {
    let mut out = Vec::new();
    if caps.create_aliases {
        out.push(cap_line(crate::i18n::ts!("permission-can-create-aliases")));
    }
    if caps.create_triggers {
        out.push(cap_line("Create triggers"));
    }
    if let Some(line) = send_can_line(caps) {
        out.push(cap_line(&line));
    }
    if caps.echo {
        out.push(cap_line(crate::i18n::ts!("permission-can-echo")));
    }
    if caps.reach_others {
        out.push(cap_line(crate::i18n::ts!("permission-can-sessions")));
    }
    if caps.change_display {
        out.push(cap_line(crate::i18n::ts!("permission-can-display")));
    }
    if caps.mapper_read {
        out.push(cap_line(crate::i18n::ts!("permission-can-map-read")));
    }
    if caps.mapper_write {
        out.push(cap_line(crate::i18n::ts!("permission-can-map-write")));
    }
    if caps.secrets_read {
        out.push(cap_line(crate::i18n::ts!("permission-can-secrets-read")));
    }
    if caps.secrets_write {
        out.push(cap_line(crate::i18n::ts!("permission-can-secrets-write")));
    }
    if caps.secrets_manage {
        out.push(cap_line(crate::i18n::ts!("permission-can-secrets-manage")));
    }
    if caps.widgets {
        out.push(cap_line(crate::i18n::ts!("permission-can-widgets")));
    }
    if caps.widget_shaders {
        out.push(cap_line(crate::i18n::ts!("permission-can-widget-shaders")));
    }
    if caps.interop_write {
        out.push(cap_line(crate::i18n::ts!("permission-can-interop-write")));
    }
    if caps.interop_read {
        out.push(cap_line(crate::i18n::ts!("permission-can-interop-read")));
    }
    if caps.interop_broadcast {
        out.push(cap_line(crate::i18n::ts!(
            "permission-can-interop-broadcast"
        )));
    }
    if caps.workers {
        // Compute-only, but a worker is a real OS thread that keeps running between
        // the isolate's turns — elevated exposure (sustained CPU), not a scoped grant.
        out.push(PermissionLine {
            head: crate::i18n::t!("permission-can-workers"),
            detail: None,
            risk: PermissionRisk::Caution,
        });
    }
    if caps.panes {
        out.push(cap_line(crate::i18n::ts!("permission-can-panes")));
    }
    if caps.gmcp_send {
        out.push(cap_line(crate::i18n::ts!("permission-can-gmcp")));
    }
    if caps.input {
        out.push(cap_line(
            "See and rewrite what you type in the command input, including \
             submitting commands and switching it into password mode",
        ));
    }
    out
}

/// The combined `send` / `send-direct` "can do" line: the wording changes with which of the two is
/// granted — both, send-only (through your aliases, which it can re-trigger), or direct-only
/// (bypassing them). `None` when neither is granted (the "cannot send" row covers that case).
fn send_can_line(caps: &SmudgyCapabilities) -> Option<String> {
    let line = match (caps.send, caps.send_direct) {
        (true, true) => crate::i18n::ts!("permission-can-send-both"),
        (true, false) => crate::i18n::ts!("permission-can-send-aliases"),
        (false, true) => crate::i18n::ts!("permission-can-send-direct"),
        (false, false) => return None,
    };
    Some(line.to_string())
}

/// Prettifies a permission path for display: the `$DATA` placeholder (host-expanded before
/// enforcement) reads as `<data>` rather than a raw env-style token.
fn pretty_path(path: &str) -> String {
    path.replace("$DATA", "<data>")
}

/// Whether a `read`/`write` path grant survives the enforcement guardrail
/// (`PACKAGE-ISOLATES-ENFORCEMENT.md`, mirroring `script_engine::expand_data_placeholder`): a
/// `$DATA/<sub>` (or `$DATA\<sub>`) whose subpath contains a `..` component is **dropped** by the
/// engine (it would let the manifest escape the data dir), so the consent window must not advertise
/// it as a capability. A bare `$DATA`, a `$DATA`-lookalike (`$DATABASE`), or a non-placeholder
/// absolute path is the author's own explicit grant and is kept.
pub(crate) fn path_grant_enforced(entry: &str) -> bool {
    let Some(rest) = entry.strip_prefix("$DATA") else {
        return true;
    };
    let sub = match rest.chars().next() {
        None => return true, // bare `$DATA`
        Some('/' | '\\') => rest.trim_start_matches(['/', '\\']),
        Some(_) => return true, // `$DATABASE` etc. — not the placeholder
    };
    !sub.split(['/', '\\']).any(|component| component == "..")
}

/// One "can do" row in the consent enumeration: a bullet, the capability label, and (when the line
/// names one) the specific host/path/var in monospace.
pub(crate) fn consent_can_row<'a, M: 'a>(line: &PermissionLine) -> Element<'a, M> {
    // The row's framing follows its risk tier: a plain scoped grant keeps the quiet accent
    // bullet; a caution line goes amber; a sandbox-escape line goes red and says so inline, so
    // the tier survives even when a caller shows rows without the full-access banner.
    let (bullet, bullet_style, head_style): (&str, _, fn(&crate::theme::Theme) -> text::Style) =
        match line.risk {
            PermissionRisk::Normal => (
                "\u{2022}",
                common::accent as fn(&crate::theme::Theme) -> text::Style,
                common::regular,
            ),
            PermissionRisk::Caution => ("\u{26A0}", common::warning, common::warning),
            PermissionRisk::Critical => ("\u{26A0}", common::danger, common::danger),
        };
    let mut r = row![
        text(bullet).size(13.0).style(bullet_style),
        text(line.head.clone()).size(13.0).style(head_style),
    ]
    .spacing(8.0)
    .align_y(Vertical::Center);
    if let Some(detail) = &line.detail {
        r = r.push(
            text(detail.clone())
                .size(12.0)
                .font(fonts::GEIST_MONO_VF)
                .style(common::muted),
        );
    }
    r.into()
}

#[cfg(test)]
mod tests {
    #[test]
    fn shader_permission_is_disclosed_separately_from_widget_creation() {
        let mut permissions = smudgy_core::models::shared_packages::PackagePermissions::default();
        permissions.smudgy.widget_shaders = true;
        let lines = super::permission_can_lines(&permissions);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].head,
            crate::i18n::t!("permission-can-widget-shaders")
        );
        permissions.smudgy.widgets = true;
        assert_eq!(super::permission_can_lines(&permissions).len(), 2);
    }

    use super::*;

    #[test]
    fn any_host_network_grants_share_the_arbitrary_import_risk_tier() {
        let wildcard = PackagePermissions {
            net: vec!["*".into(), "*:443".into()],
            ..Default::default()
        };
        let wildcard_lines = permission_can_lines(&wildcard);
        assert_eq!(wildcard_lines.len(), 2);
        assert!(
            wildcard_lines
                .iter()
                .all(|line| line.risk == PermissionRisk::Caution)
        );
        assert_eq!(union_risk(&wildcard), PermissionRisk::Caution);

        let arbitrary_import = PackagePermissions {
            import: ImportPolicy::Any,
            ..Default::default()
        };
        assert_eq!(union_risk(&arbitrary_import), PermissionRisk::Caution);

        let named_host = PackagePermissions {
            net: vec!["api.example.com:443".into()],
            ..Default::default()
        };
        assert_eq!(union_risk(&named_host), PermissionRisk::Normal);
    }

    #[test]
    fn ipc_rows_render_one_critical_line_with_both_realizations() {
        let local = PackagePermissions {
            ipc: vec![IpcEntry {
                unix: Some("/var/run/docker.sock".into()),
                windows_pipe: Some("docker_engine".into()),
            }],
            ..Default::default()
        };
        let lines = permission_can_lines(&local);
        assert_eq!(lines.len(), 1, "one row is one consent line");
        assert_eq!(
            lines[0].head,
            crate::i18n::t!("permission-connect-local-ipc")
        );
        assert_eq!(lines[0].risk, PermissionRisk::Critical);
        let detail = lines[0].detail.as_deref().expect("ipc line lists targets");
        assert!(
            detail.contains("/var/run/docker.sock") && detail.contains("docker_engine"),
            "both realizations are shown distinctly: {detail}"
        );
        // Exactly one realization is foreign to the running platform, and it is annotated.
        let foreign = crate::i18n::t!("permission-ipc-foreign-platform");
        assert_eq!(
            detail.matches(foreign.as_str()).count(),
            1,
            "the non-native realization carries the platform annotation: {detail}"
        );
        assert_eq!(union_risk(&local), PermissionRisk::Critical);
        assert_eq!(escape_reasons(&local), ["connect to local IPC services"]);

        // A row with only the foreign realization still surfaces (Critical, annotated): the
        // user is consenting to what the package gets on its other platform installs too.
        let one_sided = PackagePermissions {
            ipc: vec![IpcEntry {
                unix: if cfg!(windows) {
                    Some("/var/run/docker.sock".into())
                } else {
                    None
                },
                windows_pipe: if cfg!(windows) {
                    None
                } else {
                    Some("docker_engine".into())
                },
            }],
            ..Default::default()
        };
        let lines = permission_can_lines(&one_sided);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].risk, PermissionRisk::Critical);
        assert!(
            lines[0]
                .detail
                .as_deref()
                .expect("detail")
                .contains(foreign.as_str())
        );
    }
}
