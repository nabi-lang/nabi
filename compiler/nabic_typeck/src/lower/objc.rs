//! Objective-C availability: an API newer than the deployment target
//! is called only inside `if objc::available(macos: "13.0") { .. }`
//! (`ios: "16.0"` on iOS), as Swift's `#available` allows it.

use nabic_ast::{CallArg, Expr, ExprKind, Literal};

/// Whether the target is iOS (else macOS), and its deployment target.
type Deployment = Option<(bool, (u32, u32, u32))>;

thread_local! {
    /// `HirInput::apple` of the program being lowered.
    static DEPLOYMENT: std::cell::Cell<Deployment> = const { std::cell::Cell::new(None) };
}

pub(crate) fn set_deployment(v: Deployment) {
    DEPLOYMENT.set(v);
}

/// The build's platform as `objc::available` names it: `ios` or `macos`.
fn platform() -> &'static str {
    match DEPLOYMENT.get() {
        Some((true, _)) => "ios",
        _ => "macos",
    }
}

/// `"13.0.1"` as `(13, 0, 1)`.
pub(crate) fn version(s: &str) -> (u32, u32, u32) {
    let mut it = s.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}

fn shown((x, y, z): (u32, u32, u32)) -> String {
    if z == 0 {
        format!("{x}.{y}")
    } else {
        format!("{x}.{y}.{z}")
    }
}

/// The version an `if` condition checks for the build's platform:
/// `objc::available(macos: "13.0", ios: "16.0")` (a version alone is
/// macOS's).
pub(crate) fn guard(cond: &Expr) -> Option<(u32, u32, u32)> {
    let ExprKind::Call { callee, args, .. } = &cond.kind else {
        return None;
    };
    let ExprKind::Path(p) = &callee.kind else {
        return None;
    };
    let names: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
    if !matches!(names.as_slice(), [.., "objc", "available"]) {
        return None;
    }
    let os = platform();
    args.iter().find_map(|arg| {
        let e = match arg {
            CallArg::Positional(e) if os == "macos" => e,
            CallArg::Named { name, value } if name == os => value,
            _ => return None,
        };
        match &e.kind {
            ExprKind::Literal(Literal::Str(s)) => Some(version(s.trim_matches('"'))),
            _ => None,
        }
    })
}

/// Why a use of `name`, whose availability is `mark` (macOS's and
/// iOS's), is refused under the `guards` around it; `None` when allowed.
pub(crate) fn refused(
    name: &str,
    mark: &nabic_hir::marks::Mark,
    guards: &[(u32, u32, u32)],
) -> Option<String> {
    let (ios, deployment) = DEPLOYMENT.get()?;
    let needs = if ios {
        &mark.available_ios
    } else {
        &mark.available
    };
    let v = version(needs.as_deref()?);
    let have = guards.iter().copied().fold(deployment, Ord::max);
    if v <= have {
        return None;
    }
    let needs = shown(v);
    let now = if have > deployment {
        format!("the `objc::available` around it checks for {}", shown(have))
    } else {
        format!("the deployment target is {}", shown(deployment))
    };
    let (os, key, manifest, var) = if ios {
        (
            "iOS",
            "ios",
            format!("deployment_target: {{ ios: \"{needs}\" }}"),
            "IPHONEOS_DEPLOYMENT_TARGET",
        )
    } else {
        (
            "macOS",
            "macos",
            format!("deployment_target: \"{needs}\""),
            "MACOSX_DEPLOYMENT_TARGET",
        )
    };
    Some(format!(
        "`{name}` needs {os} {needs}, {now}: wrap it in \
         `if objc::available({key}: \"{needs}\") {{ .. }}`, or raise the deployment target \
         (`{manifest}` in nabi.nabion, or `{var}={needs}`)"
    ))
}
