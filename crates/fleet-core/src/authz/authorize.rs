//! [`authorize`]: the one function that decides, and [`AuthzError`].
//!
//! Each kind of [`ResourceContext`] has its own function. Each one matches
//! every [`Permission`] without a wildcard, so a new permission needs an
//! explicit decision for every kind of resource. The order is always:
//! whether the permission applies, whether the actor may see the resource,
//! and only then whether they may do this.

use super::{Actor, GrantLevel, ModeVisibility, Permission, ResourceContext, Role, UserRef};
use crate::id::UserId;

/// Why [`authorize`] denied a permission. Every denial tells the API what to
/// answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthzError {
    /// The actor may not even see the resource. The API answers 404, so it
    /// doesn't reveal that the resource exists (Plan.md §7.3).
    #[error("not found")]
    NotFound,
    /// The actor may see the resource, but not do this. The API answers 403.
    #[error("forbidden")]
    Forbidden,
    /// The permission doesn't apply to this kind of resource: a bug in the
    /// caller. It's denied like everything else.
    #[error("the permission doesn't apply to this kind of resource")]
    WrongResource,
}

/// Decides whether `actor` may do `permission` to `resource`.
///
/// It's pure and denies by default: only the rules in the
/// [module docs](super) allow anything.
///
/// # Errors
/// - [`AuthzError::WrongResource`] if `permission` doesn't apply to this kind
///   of resource.
/// - [`AuthzError::NotFound`] if the actor may not even see the resource.
/// - [`AuthzError::Forbidden`] if they may see it, but not do this.
pub fn authorize(
    actor: &Actor,
    permission: Permission,
    resource: &ResourceContext,
) -> Result<(), AuthzError> {
    match *resource {
        ResourceContext::Global => global(actor, permission),
        ResourceContext::Account { owner, grant } => account(actor, permission, owner, grant),
        ResourceContext::Mode { owner, visibility } => mode(actor, permission, owner, visibility),
        ResourceContext::User(target) => user(actor, permission, target),
        ResourceContext::Invite { role } => invite(actor, permission, role),
        ResourceContext::Personal { owner } => personal(actor, permission, owner),
    }
}

/// The answer for a resource the actor may see.
const fn allow(allowed: bool) -> Result<(), AuthzError> {
    if allowed {
        Ok(())
    } else {
        Err(AuthzError::Forbidden)
    }
}

/// Whether `actor` has implicit Manage over what `owner` owns: their own
/// things, everything for the Owner, and Members' things for an Admin. Not
/// the Owner's or other Admins' things for an Admin (ADR-0010). Ownership
/// goes by ID, whatever role the caller loaded for the owner.
fn manages(actor: &Actor, owner: UserRef) -> bool {
    actor.id == owner.id
        || actor.role == Role::Owner
        || (actor.role == Role::Admin && owner.role == Role::Member)
}

fn account(
    actor: &Actor,
    permission: Permission,
    owner: UserRef,
    grant: Option<GrantLevel>,
) -> Result<(), AuthzError> {
    let required = match permission {
        Permission::ViewBot => GrantLevel::View,
        Permission::ControlBot => GrantLevel::Control,
        Permission::SendChat(check) | Permission::SetBotMode(check) => {
            if check.needs_manage() {
                GrantLevel::Manage
            } else {
                GrantLevel::Control
            }
        }
        Permission::ConfigureBot
        | Permission::ManageGrants
        | Permission::RelinkAccount
        | Permission::DeleteAccount => GrantLevel::Manage,
        // Editing grants needs Manage, and nobody grants above their own
        // level (Plan.md §7.3).
        Permission::Grant { level, .. } => level.max(GrantLevel::Manage),
        Permission::LinkAccount
        | Permission::CreateMode { .. }
        | Permission::ListUsers
        | Permission::ListInvites
        | Permission::CreateInvite { .. }
        | Permission::ViewAgents
        | Permission::EnrollAgent
        | Permission::DisableAgent
        | Permission::ViewAuditLog
        | Permission::RevokeInvite
        | Permission::ViewMode
        | Permission::UpdateMode { .. }
        | Permission::DeleteMode
        | Permission::SetRole { .. }
        | Permission::SetDisabled
        | Permission::UsePersonal => return Err(AuthzError::WrongResource),
    };
    let implicit = manages(actor, owner).then_some(GrantLevel::Manage);
    let Some(level) = implicit.max(grant) else {
        return Err(AuthzError::NotFound);
    };
    // A grant to oneself would outlive the implicit Manage it came from,
    // e.g. after a Member is promoted to Admin (ADR-0010).
    let to_self = matches!(permission, Permission::Grant { grantee, .. } if grantee == actor.id);
    allow(level >= required && !to_self)
}

fn global(actor: &Actor, permission: Permission) -> Result<(), AuthzError> {
    let allowed = match permission {
        Permission::LinkAccount
        | Permission::CreateMode {
            visibility: ModeVisibility::Private,
        } => true,
        Permission::CreateMode {
            visibility: ModeVisibility::Shared,
        }
        | Permission::ListUsers
        | Permission::ListInvites
        | Permission::ViewAgents
        | Permission::DisableAgent
        | Permission::ViewAuditLog => actor.role >= Role::Admin,
        // An agent receives session tokens for the bots assigned to it, so
        // only the Owner decides which agents exist (ADR-0010).
        Permission::EnrollAgent => actor.role == Role::Owner,
        Permission::CreateInvite { role } => handles_invites_for(actor, role),
        Permission::ViewBot
        | Permission::ControlBot
        | Permission::SendChat(_)
        | Permission::SetBotMode(_)
        | Permission::ConfigureBot
        | Permission::ManageGrants
        | Permission::Grant { .. }
        | Permission::RelinkAccount
        | Permission::DeleteAccount
        | Permission::RevokeInvite
        | Permission::ViewMode
        | Permission::UpdateMode { .. }
        | Permission::DeleteMode
        | Permission::SetRole { .. }
        | Permission::SetDisabled
        | Permission::UsePersonal => return Err(AuthzError::WrongResource),
    };
    allow(allowed)
}

/// Who creates and revokes invites for `role`: Admins for Members, the Owner
/// for Admins, and nobody for an Owner (Plan.md §7.2).
fn handles_invites_for(actor: &Actor, role: Role) -> bool {
    match role {
        Role::Member => actor.role >= Role::Admin,
        Role::Admin => actor.role == Role::Owner,
        Role::Owner => false,
    }
}

fn invite(actor: &Actor, permission: Permission, role: Role) -> Result<(), AuthzError> {
    match permission {
        Permission::RevokeInvite => {}
        Permission::ViewBot
        | Permission::ControlBot
        | Permission::SendChat(_)
        | Permission::SetBotMode(_)
        | Permission::ConfigureBot
        | Permission::ManageGrants
        | Permission::Grant { .. }
        | Permission::RelinkAccount
        | Permission::DeleteAccount
        | Permission::LinkAccount
        | Permission::CreateMode { .. }
        | Permission::ListUsers
        | Permission::ListInvites
        | Permission::CreateInvite { .. }
        | Permission::ViewAgents
        | Permission::EnrollAgent
        | Permission::DisableAgent
        | Permission::ViewAuditLog
        | Permission::ViewMode
        | Permission::UpdateMode { .. }
        | Permission::DeleteMode
        | Permission::SetRole { .. }
        | Permission::SetDisabled
        | Permission::UsePersonal => return Err(AuthzError::WrongResource),
    }
    if actor.role < Role::Admin {
        return Err(AuthzError::NotFound);
    }
    allow(handles_invites_for(actor, role))
}

fn mode(
    actor: &Actor,
    permission: Permission,
    owner: Option<UserRef>,
    visibility: ModeVisibility,
) -> Result<(), AuthzError> {
    // The visibility the actor must be able to edit the mode with after the
    // change, if the permission changes it.
    let edited = match permission {
        Permission::ViewMode => None,
        Permission::UpdateMode { visibility: after } => Some(after),
        Permission::DeleteMode => Some(visibility),
        Permission::ViewBot
        | Permission::ControlBot
        | Permission::SendChat(_)
        | Permission::SetBotMode(_)
        | Permission::ConfigureBot
        | Permission::ManageGrants
        | Permission::Grant { .. }
        | Permission::RelinkAccount
        | Permission::DeleteAccount
        | Permission::LinkAccount
        | Permission::CreateMode { .. }
        | Permission::ListUsers
        | Permission::ListInvites
        | Permission::CreateInvite { .. }
        | Permission::ViewAgents
        | Permission::EnrollAgent
        | Permission::DisableAgent
        | Permission::ViewAuditLog
        | Permission::RevokeInvite
        | Permission::SetRole { .. }
        | Permission::SetDisabled
        | Permission::UsePersonal => return Err(AuthzError::WrongResource),
    };
    let visible = match (owner, visibility) {
        (None, _) | (Some(_), ModeVisibility::Shared) => true,
        (Some(owner), ModeVisibility::Private) => manages(actor, owner),
    };
    if !visible {
        return Err(AuthzError::NotFound);
    }
    // Editing needs the rights both as the mode is and as it will be, so an
    // Admin can't share a Member's private mode.
    allow(edited.is_none_or(|after| {
        edits_mode(actor, owner, visibility) && edits_mode(actor, owner, after)
    }))
}

/// Whether `actor` may edit a mode of `owner` with `visibility`. Built-in
/// modes are read-only for everyone. A private mode follows the account
/// rule. A shared mode is its creator's while they're an Admin, and the
/// Owner's: shared modes need Admin+ (Plan.md Appendix B).
fn edits_mode(actor: &Actor, owner: Option<UserRef>, visibility: ModeVisibility) -> bool {
    let Some(owner) = owner else {
        return false;
    };
    match visibility {
        ModeVisibility::Private => manages(actor, owner),
        ModeVisibility::Shared => {
            actor.role == Role::Owner || (actor.id == owner.id && actor.role >= Role::Admin)
        }
    }
}

fn user(actor: &Actor, permission: Permission, target: UserRef) -> Result<(), AuthzError> {
    let allowed = match permission {
        // Only the Owner manages Admins, so only the Owner changes roles.
        // Nobody becomes the Owner this way.
        Permission::SetRole { role } => {
            actor.role == Role::Owner && role != Role::Owner && manages_user(actor, target)
        }
        Permission::SetDisabled => manages_user(actor, target),
        Permission::ViewBot
        | Permission::ControlBot
        | Permission::SendChat(_)
        | Permission::SetBotMode(_)
        | Permission::ConfigureBot
        | Permission::ManageGrants
        | Permission::Grant { .. }
        | Permission::RelinkAccount
        | Permission::DeleteAccount
        | Permission::LinkAccount
        | Permission::CreateMode { .. }
        | Permission::ListUsers
        | Permission::ListInvites
        | Permission::CreateInvite { .. }
        | Permission::ViewAgents
        | Permission::EnrollAgent
        | Permission::DisableAgent
        | Permission::ViewAuditLog
        | Permission::RevokeInvite
        | Permission::ViewMode
        | Permission::UpdateMode { .. }
        | Permission::DeleteMode
        | Permission::UsePersonal => return Err(AuthzError::WrongResource),
    };
    // Admins list all users; a Member sees only themselves.
    if actor.id != target.id && actor.role < Role::Admin {
        return Err(AuthzError::NotFound);
    }
    allow(allowed)
}

/// Whether `actor` may act on the user `target`: never on themselves or an
/// Owner, and as an Admin only on Members. Another user with the Owner role
/// can't exist (P7.1), and is refused if it does.
fn manages_user(actor: &Actor, target: UserRef) -> bool {
    actor.id != target.id
        && target.role != Role::Owner
        && match actor.role {
            Role::Owner => true,
            Role::Admin => target.role == Role::Member,
            Role::Member => false,
        }
}

fn personal(actor: &Actor, permission: Permission, owner: UserId) -> Result<(), AuthzError> {
    match permission {
        Permission::UsePersonal => {}
        Permission::ViewBot
        | Permission::ControlBot
        | Permission::SendChat(_)
        | Permission::SetBotMode(_)
        | Permission::ConfigureBot
        | Permission::ManageGrants
        | Permission::Grant { .. }
        | Permission::RelinkAccount
        | Permission::DeleteAccount
        | Permission::LinkAccount
        | Permission::CreateMode { .. }
        | Permission::ListUsers
        | Permission::ListInvites
        | Permission::CreateInvite { .. }
        | Permission::ViewAgents
        | Permission::EnrollAgent
        | Permission::DisableAgent
        | Permission::ViewAuditLog
        | Permission::RevokeInvite
        | Permission::ViewMode
        | Permission::UpdateMode { .. }
        | Permission::DeleteMode
        | Permission::SetRole { .. }
        | Permission::SetDisabled => return Err(AuthzError::WrongResource),
    }
    // Not even the Owner sees another user's sessions or link flows.
    if actor.id == owner {
        Ok(())
    } else {
        Err(AuthzError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::{CommandAllowlist, CommandCheck, GrantLevel, ModeVisibility, Role, UserRef};
    use crate::chat::ChatMessage;
    use crate::id::UserId;
    use crate::mode::{Action, ModeDefinition, ModeDraft, Schedule, Step};
    use chrono::DateTime;
    use core::fmt::Write as _;
    use rstest::rstest;
    use std::collections::BTreeSet;

    use AuthzError::{Forbidden, NotFound, WrongResource};
    use GrantLevel::{Control, Manage, View};
    use ModeVisibility::{Private, Shared};

    // Fixtures.

    /// The actor's ID in every test.
    const ME: u8 = 1;
    /// The ID of the user who owns the resource or is its target, if that
    /// isn't the actor.
    const OTHER: u8 = 2;
    /// The ID of a third user, who receives grants.
    const THIRD: u8 = 3;

    fn user_id(n: u8) -> UserId {
        UserId::new_v7(DateTime::from_timestamp(1_800_000_000, 0).unwrap(), [n; 10]).unwrap()
    }

    fn actor(role: Role) -> Actor {
        Actor {
            id: user_id(ME),
            role,
        }
    }

    fn myself(actor: &Actor) -> UserRef {
        UserRef {
            id: actor.id,
            role: actor.role,
        }
    }

    fn other(role: Role) -> UserRef {
        UserRef {
            id: user_id(OTHER),
            role,
        }
    }

    const fn account(owner: UserRef, grant: Option<GrantLevel>) -> ResourceContext {
        ResourceContext::Account { owner, grant }
    }

    fn grant_to(level: GrantLevel, grantee: u8) -> Permission {
        Permission::Grant {
            level,
            grantee: user_id(grantee),
        }
    }

    fn spawn_only() -> CommandAllowlist {
        CommandAllowlist::try_new(&["/spawn"]).unwrap()
    }

    /// Checks a chat message against an allowlist with only `/spawn`.
    fn chat(text: &str) -> CommandCheck {
        spawn_only().check(&ChatMessage::try_from(text).unwrap())
    }

    /// A mode that sends `text` once at the start.
    fn chat_mode(text: &str) -> ModeDefinition {
        ModeDraft {
            steps: vec![Step {
                action: Action::SendChat {
                    message: ChatMessage::try_from(text).unwrap(),
                },
                schedule: Schedule::AtStart,
                probability: 100,
            }],
        }
        .validate()
        .unwrap()
    }

    // What the matrix covers.

    /// The kinds of [`ResourceContext`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    enum Kind {
        Global,
        Account,
        Mode,
        User,
        Invite,
        Personal,
    }

    /// The kind of `context`. The match has no wildcard, so a new kind of
    /// context doesn't compile until it's added here.
    const fn kind_of(context: &ResourceContext) -> Kind {
        match context {
            ResourceContext::Global => Kind::Global,
            ResourceContext::Account { .. } => Kind::Account,
            ResourceContext::Mode { .. } => Kind::Mode,
            ResourceContext::User(_) => Kind::User,
            ResourceContext::Invite { .. } => Kind::Invite,
            ResourceContext::Personal { .. } => Kind::Personal,
        }
    }

    /// How many variants [`Permission`] has.
    const PERMISSION_VARIANTS: usize = 25;

    /// The variant's index, and the kind of context it applies to.
    ///
    /// The match has no wildcard: a new variant doesn't compile until it gets
    /// the next index here, and `the_matrix_covers_every_permission` fails
    /// until `permission_cases` lists it.
    const fn variant(permission: Permission) -> (usize, Kind) {
        match permission {
            Permission::ViewBot => (0, Kind::Account),
            Permission::ControlBot => (1, Kind::Account),
            Permission::SendChat(_) => (2, Kind::Account),
            Permission::SetBotMode(_) => (3, Kind::Account),
            Permission::ConfigureBot => (4, Kind::Account),
            Permission::ManageGrants => (5, Kind::Account),
            Permission::Grant { .. } => (6, Kind::Account),
            Permission::RelinkAccount => (7, Kind::Account),
            Permission::DeleteAccount => (8, Kind::Account),
            Permission::LinkAccount => (9, Kind::Global),
            Permission::CreateMode { .. } => (10, Kind::Global),
            Permission::ListUsers => (11, Kind::Global),
            Permission::ListInvites => (12, Kind::Global),
            Permission::CreateInvite { .. } => (13, Kind::Global),
            Permission::ViewAgents => (14, Kind::Global),
            Permission::EnrollAgent => (15, Kind::Global),
            Permission::DisableAgent => (16, Kind::Global),
            Permission::ViewAuditLog => (17, Kind::Global),
            Permission::RevokeInvite => (18, Kind::Invite),
            Permission::ViewMode => (19, Kind::Mode),
            Permission::UpdateMode { .. } => (20, Kind::Mode),
            Permission::DeleteMode => (21, Kind::Mode),
            Permission::SetRole { .. } => (22, Kind::User),
            Permission::SetDisabled => (23, Kind::User),
            Permission::UsePersonal => (24, Kind::Personal),
        }
    }

    /// Every permission the matrix shows: each variant, with each value of
    /// its parameters that the rules tell apart.
    fn permission_cases() -> Vec<(&'static str, Permission)> {
        let listed = chat("/spawn");
        let unlisted = chat("/op AfkBot1");
        vec![
            ("ViewBot", Permission::ViewBot),
            ("ControlBot", Permission::ControlBot),
            (
                "SendChat: plain chat or an allowlisted command",
                Permission::SendChat(listed),
            ),
            (
                "SendChat: a command off the allowlist",
                Permission::SendChat(unlisted),
            ),
            (
                "SetBotMode: no command off the allowlist",
                Permission::SetBotMode(listed),
            ),
            (
                "SetBotMode: a command off the allowlist",
                Permission::SetBotMode(unlisted),
            ),
            ("ConfigureBot", Permission::ConfigureBot),
            ("ManageGrants", Permission::ManageGrants),
            ("Grant view to another user", grant_to(View, THIRD)),
            ("Grant control to another user", grant_to(Control, THIRD)),
            ("Grant manage to another user", grant_to(Manage, THIRD)),
            ("Grant view to oneself", grant_to(View, ME)),
            ("RelinkAccount", Permission::RelinkAccount),
            ("DeleteAccount", Permission::DeleteAccount),
            ("LinkAccount", Permission::LinkAccount),
            (
                "CreateMode private",
                Permission::CreateMode {
                    visibility: Private,
                },
            ),
            (
                "CreateMode shared",
                Permission::CreateMode { visibility: Shared },
            ),
            ("ListUsers", Permission::ListUsers),
            ("ListInvites", Permission::ListInvites),
            (
                "CreateInvite for a Member",
                Permission::CreateInvite { role: Role::Member },
            ),
            (
                "CreateInvite for an Admin",
                Permission::CreateInvite { role: Role::Admin },
            ),
            (
                "CreateInvite for an Owner",
                Permission::CreateInvite { role: Role::Owner },
            ),
            ("ViewAgents", Permission::ViewAgents),
            ("EnrollAgent", Permission::EnrollAgent),
            ("DisableAgent", Permission::DisableAgent),
            ("ViewAuditLog", Permission::ViewAuditLog),
            ("RevokeInvite", Permission::RevokeInvite),
            ("ViewMode", Permission::ViewMode),
            (
                "UpdateMode to private",
                Permission::UpdateMode {
                    visibility: Private,
                },
            ),
            (
                "UpdateMode to shared",
                Permission::UpdateMode { visibility: Shared },
            ),
            ("DeleteMode", Permission::DeleteMode),
            ("SetRole member", Permission::SetRole { role: Role::Member }),
            ("SetRole admin", Permission::SetRole { role: Role::Admin }),
            ("SetRole owner", Permission::SetRole { role: Role::Owner }),
            ("SetDisabled", Permission::SetDisabled),
            ("UsePersonal", Permission::UsePersonal),
        ]
    }

    fn cases_for(kind: Kind) -> Vec<(&'static str, Permission)> {
        permission_cases()
            .into_iter()
            .filter(|&(_, permission)| variant(permission).1 == kind)
            .collect()
    }

    const ROLES: [Role; 3] = [Role::Owner, Role::Admin, Role::Member];
    const GRANTS: [Option<GrantLevel>; 4] = [None, Some(View), Some(Control), Some(Manage)];

    /// Who owns the resource or is its target, seen from the actor.
    #[derive(Debug, Clone, Copy)]
    enum Relation {
        Own,
        Other(Role),
    }

    /// The relations the matrix shows for an actor with `role`. There's only
    /// one Owner (P7.1), so the matrix leaves out an Owner meeting another
    /// Owner; `another_owner_fails_closed` covers it.
    fn relations(role: Role) -> Vec<Relation> {
        let mut relations = vec![Relation::Own];
        relations.extend(
            [Role::Member, Role::Admin, Role::Owner]
                .into_iter()
                .filter(|&other| !(role == Role::Owner && other == Role::Owner))
                .map(Relation::Other),
        );
        relations
    }

    fn party(actor: &Actor, relation: Relation) -> UserRef {
        match relation {
            Relation::Own => myself(actor),
            Relation::Other(role) => other(role),
        }
    }

    const fn role_name(role: Role) -> &'static str {
        match role {
            Role::Owner => "Owner",
            Role::Admin => "Admin",
            Role::Member => "Member",
        }
    }

    fn relation_name(actor: Role, relation: Relation) -> &'static str {
        match relation {
            Relation::Own => "self",
            Relation::Other(role) => match (role == actor, role) {
                (true, Role::Member) => "another Member",
                (true, Role::Admin) => "another Admin",
                (true, Role::Owner) => "another Owner",
                (false, Role::Member) => "a Member",
                (false, Role::Admin) => "an Admin",
                (false, Role::Owner) => "the Owner",
            },
        }
    }

    // The matrix.

    fn cell(result: Result<(), AuthzError>) -> String {
        match result {
            Ok(()) => "ok",
            Err(NotFound) => "404",
            Err(Forbidden) => "403",
            Err(WrongResource) => "wrong resource",
        }
        .to_owned()
    }

    /// Renders a header and rows as aligned columns.
    fn table(header: &[&str], rows: &[Vec<String>]) -> String {
        let mut widths: Vec<usize> = header.iter().map(|title| title.chars().count()).collect();
        for row in rows {
            for (width, text) in widths.iter_mut().zip(row) {
                *width = (*width).max(text.chars().count());
            }
        }
        let line = |cells: &[&str]| {
            let padded: Vec<String> = cells
                .iter()
                .zip(&widths)
                .map(|(text, &width)| format!("{text:<width$}"))
                .collect();
            format!("{}\n", padded.join("  ").trim_end())
        };
        let mut out = line(header);
        for row in rows {
            let cells: Vec<&str> = row.iter().map(String::as_str).collect();
            out.push_str(&line(&cells));
        }
        out
    }

    fn account_section() -> String {
        let mut out = String::from(
            "## Accounts and bots\n\
             Rows: the actor's role, and whose account it is. Columns: the grant the actor holds on it.\n",
        );
        for (label, permission) in cases_for(Kind::Account) {
            let mut rows = Vec::new();
            for role in ROLES {
                let actor = actor(role);
                for relation in relations(role) {
                    let mut row = vec![
                        role_name(role).to_owned(),
                        relation_name(role, relation).to_owned(),
                    ];
                    let owner = party(&actor, relation);
                    row.extend(
                        GRANTS.map(|grant| {
                            cell(authorize(&actor, permission, &account(owner, grant)))
                        }),
                    );
                    rows.push(row);
                }
            }
            let header = ["actor", "owner", "no grant", "view", "control", "manage"];
            write!(out, "\n### {label}\n{}", table(&header, &rows)).unwrap();
        }
        out
    }

    fn global_section() -> String {
        let rows: Vec<Vec<String>> = cases_for(Kind::Global)
            .into_iter()
            .map(|(label, permission)| {
                let mut row = vec![label.to_owned()];
                row.extend(ROLES.map(|role| {
                    cell(authorize(
                        &actor(role),
                        permission,
                        &ResourceContext::Global,
                    ))
                }));
                row
            })
            .collect();
        format!(
            "## Fleet-wide\nColumns: the actor's role.\n\n{}",
            table(&["permission", "Owner", "Admin", "Member"], &rows)
        )
    }

    fn invite_section() -> String {
        let mut out = String::from(
            "## Invites\nRows: the role the invite gives. Columns: the actor's role.\n",
        );
        for (label, permission) in cases_for(Kind::Invite) {
            let rows: Vec<Vec<String>> = ROLES
                .into_iter()
                .rev()
                .map(|invited| {
                    let mut row = vec![role_name(invited).to_owned()];
                    row.extend(ROLES.map(|role| {
                        let invite = ResourceContext::Invite { role: invited };
                        cell(authorize(&actor(role), permission, &invite))
                    }));
                    row
                })
                .collect();
            let header = ["invite for", "Owner", "Admin", "Member"];
            write!(out, "\n### {label}\n{}", table(&header, &rows)).unwrap();
        }
        out
    }

    fn mode_section() -> String {
        let cases = cases_for(Kind::Mode);
        let mut rows = Vec::new();
        for role in ROLES {
            let actor = actor(role);
            let mut modes = vec![(
                "built-in".to_owned(),
                ResourceContext::Mode {
                    owner: None,
                    visibility: Shared,
                },
            )];
            for relation in relations(role) {
                for (visibility, label) in [(Private, "private"), (Shared, "shared")] {
                    let name = match relation {
                        Relation::Own => format!("own {label}"),
                        Relation::Other(_) => {
                            format!("{}'s {label}", relation_name(role, relation))
                        }
                    };
                    let owner = Some(party(&actor, relation));
                    modes.push((name, ResourceContext::Mode { owner, visibility }));
                }
            }
            for (name, mode) in modes {
                let mut row = vec![role_name(role).to_owned(), name];
                row.extend(
                    cases
                        .iter()
                        .map(|&(_, permission)| cell(authorize(&actor, permission, &mode))),
                );
                rows.push(row);
            }
        }
        let mut header = vec!["actor", "mode"];
        header.extend(cases.iter().map(|&(label, _)| label));
        format!(
            "## Modes\nRows: the actor's role, and whose mode it is with its visibility. Columns: the permission.\n\n{}",
            table(&header, &rows)
        )
    }

    fn user_section() -> String {
        let cases = cases_for(Kind::User);
        let mut rows = Vec::new();
        for role in ROLES {
            let actor = actor(role);
            for relation in relations(role) {
                let target = ResourceContext::User(party(&actor, relation));
                let mut row = vec![
                    role_name(role).to_owned(),
                    relation_name(role, relation).to_owned(),
                ];
                row.extend(
                    cases
                        .iter()
                        .map(|&(_, permission)| cell(authorize(&actor, permission, &target))),
                );
                rows.push(row);
            }
        }
        let mut header = vec!["actor", "target"];
        header.extend(cases.iter().map(|&(label, _)| label));
        format!(
            "## Users\nRows: the actor's role, and the target user. Columns: the permission.\n\n{}",
            table(&header, &rows)
        )
    }

    fn personal_section() -> String {
        let cases = cases_for(Kind::Personal);
        let mut rows = Vec::new();
        for role in ROLES {
            let actor = actor(role);
            for (name, owner) in [("self", actor.id), ("another user", user_id(OTHER))] {
                let personal = ResourceContext::Personal { owner };
                let mut row = vec![role_name(role).to_owned(), name.to_owned()];
                row.extend(
                    cases
                        .iter()
                        .map(|&(_, permission)| cell(authorize(&actor, permission, &personal))),
                );
                rows.push(row);
            }
        }
        let mut header = vec!["actor", "belongs to"];
        header.extend(cases.iter().map(|&(label, _)| label));
        format!(
            "## Personal resources\nRows: the actor's role, and whom the resource belongs to. Columns: the permission.\n\n{}",
            table(&header, &rows)
        )
    }

    fn matrix() -> String {
        [
            "# Authorization matrix\n\
             Generated from `authorize` by its tests. ok = allowed, 403 = Forbidden, 404 = NotFound.\n"
                .to_owned(),
            account_section(),
            global_section(),
            invite_section(),
            mode_section(),
            user_section(),
            personal_section(),
        ]
        .join("\n")
    }

    #[test]
    fn authorization_matrix() {
        insta::assert_snapshot!("authorization_matrix", matrix());
    }

    #[test]
    fn the_matrix_covers_every_permission() {
        let covered: BTreeSet<usize> = permission_cases()
            .into_iter()
            .map(|(_, permission)| variant(permission).0)
            .collect();

        assert_eq!(covered, (0..PERMISSION_VARIANTS).collect());
    }

    // Deny by default.

    /// One context of each kind that would allow the actor everything that
    /// applies to it, so only the kind can be wrong.
    fn permissive_contexts(actor: &Actor) -> [ResourceContext; 6] {
        [
            ResourceContext::Global,
            account(myself(actor), Some(Manage)),
            ResourceContext::Mode {
                owner: Some(myself(actor)),
                visibility: Private,
            },
            ResourceContext::User(other(Role::Member)),
            ResourceContext::Invite { role: Role::Member },
            ResourceContext::Personal { owner: actor.id },
        ]
    }

    #[test]
    fn a_permission_on_the_wrong_kind_of_resource_is_denied() {
        for role in ROLES {
            let actor = actor(role);
            let contexts = permissive_contexts(&actor);
            let kinds: BTreeSet<Kind> = contexts.iter().map(kind_of).collect();
            assert_eq!(kinds.len(), contexts.len(), "one context of each kind");

            for (label, permission) in permission_cases() {
                for context in &contexts {
                    if kind_of(context) != variant(permission).1 {
                        assert_eq!(
                            authorize(&actor, permission, context),
                            Err(WrongResource),
                            "{role:?}: {label} on {context:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_higher_grant_never_allows_less() {
        for role in ROLES {
            let actor = actor(role);
            for relation in relations(role) {
                let owner = party(&actor, relation);
                for (label, permission) in cases_for(Kind::Account) {
                    let allowed = GRANTS
                        .map(|grant| authorize(&actor, permission, &account(owner, grant)).is_ok());
                    assert!(
                        allowed.windows(2).all(|pair| !pair[0] || pair[1]),
                        "{role:?} on {relation:?}: {label} {allowed:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn an_account_hidden_from_view_is_hidden_from_every_permission() {
        for role in ROLES {
            let actor = actor(role);
            for relation in relations(role) {
                for grant in GRANTS {
                    let account = account(party(&actor, relation), grant);
                    let hidden = authorize(&actor, Permission::ViewBot, &account) == Err(NotFound);
                    for (label, permission) in cases_for(Kind::Account) {
                        assert_eq!(
                            authorize(&actor, permission, &account) == Err(NotFound),
                            hidden,
                            "{role:?} on {relation:?} with {grant:?}: {label}"
                        );
                    }
                }
            }
        }
    }

    // Accounts: Admins and the Owner.

    #[rstest]
    #[case::the_owners(Role::Owner)]
    #[case::another_admins(Role::Admin)]
    fn an_admin_has_no_implicit_access_to_the_owners_or_another_admins_account(
        #[case] owner: Role,
    ) {
        let admin = actor(Role::Admin);
        let account = account(other(owner), None);

        assert_eq!(
            authorize(&admin, Permission::ViewBot, &account),
            Err(NotFound)
        );
        assert_eq!(
            authorize(&admin, Permission::DeleteAccount, &account),
            Err(NotFound)
        );
    }

    #[rstest]
    #[case::the_owners(Role::Owner)]
    #[case::another_admins(Role::Admin)]
    fn an_admin_with_a_grant_on_the_owners_or_another_admins_account_gets_only_that_grant(
        #[case] owner: Role,
    ) {
        let admin = actor(Role::Admin);
        let account = account(other(owner), Some(Control));

        assert_eq!(authorize(&admin, Permission::ControlBot, &account), Ok(()));
        assert_eq!(
            authorize(&admin, Permission::ConfigureBot, &account),
            Err(Forbidden)
        );
        assert_eq!(
            authorize(&admin, grant_to(View, THIRD), &account),
            Err(Forbidden)
        );
    }

    #[rstest]
    #[case::own(Relation::Own)]
    #[case::a_members(Relation::Other(Role::Member))]
    fn an_admin_has_manage_on_members_accounts_and_their_own(#[case] relation: Relation) {
        let admin = actor(Role::Admin);
        let account = account(party(&admin, relation), None);

        assert_eq!(
            authorize(&admin, Permission::DeleteAccount, &account),
            Ok(())
        );
        assert_eq!(authorize(&admin, grant_to(Manage, THIRD), &account), Ok(()));
    }

    #[rstest]
    #[case::a_members(Role::Member)]
    #[case::an_admins(Role::Admin)]
    fn the_owner_has_manage_on_every_account(#[case] owner: Role) {
        let account = account(other(owner), None);

        assert_eq!(
            authorize(&actor(Role::Owner), Permission::DeleteAccount, &account),
            Ok(())
        );
    }

    #[test]
    fn ownership_is_decided_by_id_even_if_the_role_is_stale() {
        let admin = actor(Role::Admin);
        let stale = UserRef {
            id: admin.id,
            role: Role::Owner,
        };

        assert_eq!(
            authorize(&admin, Permission::DeleteAccount, &account(stale, None)),
            Ok(())
        );
    }

    // Accounts: grants.

    #[rstest]
    #[case::view_grants_view(View, View)]
    #[case::control_grants_view(Control, View)]
    #[case::control_grants_control(Control, Control)]
    #[case::control_grants_manage(Control, Manage)]
    fn nobody_below_manage_grants(#[case] held: GrantLevel, #[case] granted: GrantLevel) {
        let account = account(other(Role::Member), Some(held));

        assert_eq!(
            authorize(&actor(Role::Member), grant_to(granted, THIRD), &account),
            Err(Forbidden)
        );
    }

    #[rstest]
    #[case::view(View)]
    #[case::control(Control)]
    #[case::manage(Manage)]
    fn manage_grants_up_to_manage(#[case] granted: GrantLevel) {
        let account = account(other(Role::Member), Some(Manage));

        assert_eq!(
            authorize(&actor(Role::Member), grant_to(granted, THIRD), &account),
            Ok(())
        );
    }

    #[rstest]
    #[case::owner(Role::Owner)]
    #[case::admin(Role::Admin)]
    #[case::member(Role::Member)]
    fn nobody_grants_to_themselves(#[case] role: Role) {
        let actor = actor(role);
        let own = account(myself(&actor), None);
        let a_members = account(other(Role::Member), Some(Manage));

        for level in [View, Control, Manage] {
            assert_eq!(authorize(&actor, grant_to(level, ME), &own), Err(Forbidden));
            assert_eq!(
                authorize(&actor, grant_to(level, ME), &a_members),
                Err(Forbidden)
            );
        }
    }

    #[test]
    fn a_member_without_a_grant_sees_nothing_of_another_members_account() {
        let member = actor(Role::Member);
        let account = account(other(Role::Member), None);

        for (label, permission) in cases_for(Kind::Account) {
            assert_eq!(
                authorize(&member, permission, &account),
                Err(NotFound),
                "{label}"
            );
        }
    }

    // Accounts: commands.

    #[rstest]
    #[case::listed_with_arguments("/spawn home", Ok(()))]
    #[case::plain_chat("hello", Ok(()))]
    #[case::wrong_case("/Spawn", Err(Forbidden))]
    #[case::namespaced("/minecraft:spawn", Err(Forbidden))]
    #[case::zero_width_space("/spawn\u{200B}", Err(Forbidden))]
    #[case::other_command("/op AfkBot1", Err(Forbidden))]
    fn with_control_only_allowlisted_commands_get_through(
        #[case] text: &str,
        #[case] expected: Result<(), AuthzError>,
    ) {
        let account = account(other(Role::Member), Some(Control));

        assert_eq!(
            authorize(
                &actor(Role::Member),
                Permission::SendChat(chat(text)),
                &account
            ),
            expected
        );
    }

    #[test]
    fn manage_sends_any_command() {
        let account = account(other(Role::Member), Some(Manage));

        assert_eq!(
            authorize(
                &actor(Role::Member),
                Permission::SendChat(chat("/op AfkBot1")),
                &account
            ),
            Ok(())
        );
    }

    #[rstest]
    #[case::afk(ModeDefinition::afk(), Ok(()))]
    #[case::with_a_listed_command(chat_mode("/spawn"), Ok(()))]
    #[case::with_an_unlisted_command(chat_mode("/home"), Err(Forbidden))]
    fn with_control_a_mode_with_a_command_off_the_allowlist_is_denied(
        #[case] mode: ModeDefinition,
        #[case] expected: Result<(), AuthzError>,
    ) {
        let account = account(other(Role::Member), Some(Control));
        let permission = Permission::SetBotMode(spawn_only().check_mode(&mode));

        assert_eq!(
            authorize(&actor(Role::Member), permission, &account),
            expected
        );
    }

    // Fleet-wide and invites.

    #[rstest]
    #[case::owner(Role::Owner, Ok(()))]
    #[case::admin(Role::Admin, Err(Forbidden))]
    #[case::member(Role::Member, Err(Forbidden))]
    fn only_the_owner_enrolls_agents(#[case] role: Role, #[case] expected: Result<(), AuthzError>) {
        assert_eq!(
            authorize(
                &actor(role),
                Permission::EnrollAgent,
                &ResourceContext::Global
            ),
            expected
        );
    }

    #[test]
    fn nobody_creates_or_revokes_an_owner_invite() {
        let owner = actor(Role::Owner);
        let create = Permission::CreateInvite { role: Role::Owner };
        let invite = ResourceContext::Invite { role: Role::Owner };

        assert_eq!(
            authorize(&owner, create, &ResourceContext::Global),
            Err(Forbidden)
        );
        assert_eq!(
            authorize(&owner, Permission::RevokeInvite, &invite),
            Err(Forbidden)
        );
    }

    #[test]
    fn admins_create_and_revoke_only_member_invites() {
        let admin = actor(Role::Admin);

        for (role, expected) in [(Role::Member, Ok(())), (Role::Admin, Err(Forbidden))] {
            let create = Permission::CreateInvite { role };
            let invite = ResourceContext::Invite { role };
            assert_eq!(
                authorize(&admin, create, &ResourceContext::Global),
                expected
            );
            assert_eq!(
                authorize(&admin, Permission::RevokeInvite, &invite),
                expected
            );
        }
    }

    // Modes.

    #[rstest]
    #[case::owner(Role::Owner)]
    #[case::admin(Role::Admin)]
    #[case::member(Role::Member)]
    fn built_in_modes_are_read_only_for_everyone(#[case] role: Role) {
        let actor = actor(role);

        for visibility in [Private, Shared] {
            let built_in = ResourceContext::Mode {
                owner: None,
                visibility,
            };
            assert_eq!(authorize(&actor, Permission::ViewMode, &built_in), Ok(()));
            assert_eq!(
                authorize(&actor, Permission::DeleteMode, &built_in),
                Err(Forbidden)
            );
            for after in [Private, Shared] {
                let update = Permission::UpdateMode { visibility: after };
                assert_eq!(authorize(&actor, update, &built_in), Err(Forbidden));
            }
        }
    }

    #[test]
    fn an_admin_edits_a_members_private_mode_but_cannot_share_it() {
        let admin = actor(Role::Admin);
        let mode = ResourceContext::Mode {
            owner: Some(other(Role::Member)),
            visibility: Private,
        };

        let update = |visibility| Permission::UpdateMode { visibility };
        assert_eq!(authorize(&admin, update(Private), &mode), Ok(()));
        assert_eq!(authorize(&admin, update(Shared), &mode), Err(Forbidden));
    }

    #[rstest]
    #[case::the_owners(Role::Owner)]
    #[case::another_admins(Role::Admin)]
    fn an_admin_cannot_see_the_owners_or_another_admins_private_mode(#[case] owner: Role) {
        let mode = ResourceContext::Mode {
            owner: Some(other(owner)),
            visibility: Private,
        };

        assert_eq!(
            authorize(&actor(Role::Admin), Permission::ViewMode, &mode),
            Err(NotFound)
        );
    }

    #[test]
    fn an_admin_cannot_edit_another_admins_shared_mode() {
        let mode = ResourceContext::Mode {
            owner: Some(other(Role::Admin)),
            visibility: Shared,
        };

        assert_eq!(
            authorize(&actor(Role::Admin), Permission::DeleteMode, &mode),
            Err(Forbidden)
        );
    }

    #[test]
    fn a_creator_demoted_to_member_can_no_longer_edit_their_shared_mode() {
        let member = actor(Role::Member);
        let mode = ResourceContext::Mode {
            owner: Some(myself(&member)),
            visibility: Shared,
        };

        assert_eq!(authorize(&member, Permission::ViewMode, &mode), Ok(()));
        assert_eq!(
            authorize(&member, Permission::DeleteMode, &mode),
            Err(Forbidden)
        );
        for visibility in [Private, Shared] {
            let update = Permission::UpdateMode { visibility };
            assert_eq!(authorize(&member, update, &mode), Err(Forbidden));
        }
    }

    // Users.

    #[test]
    fn another_owner_fails_closed() {
        let owner = actor(Role::Owner);
        let target = ResourceContext::User(other(Role::Owner));

        assert_eq!(
            authorize(&owner, Permission::SetDisabled, &target),
            Err(Forbidden)
        );
        for role in [Role::Member, Role::Admin] {
            let set_role = Permission::SetRole { role };
            assert_eq!(authorize(&owner, set_role, &target), Err(Forbidden));
        }
    }

    #[rstest]
    #[case::promote_a_member(Role::Member, Role::Admin)]
    #[case::demote_an_admin(Role::Admin, Role::Member)]
    #[case::demote_the_owner(Role::Owner, Role::Admin)]
    fn admins_cannot_change_roles(#[case] target: Role, #[case] role: Role) {
        let target = ResourceContext::User(other(target));

        assert_eq!(
            authorize(&actor(Role::Admin), Permission::SetRole { role }, &target),
            Err(Forbidden)
        );
    }

    #[rstest]
    #[case::a_member(Role::Member, Ok(()))]
    #[case::another_admin(Role::Admin, Err(Forbidden))]
    #[case::the_owner(Role::Owner, Err(Forbidden))]
    fn admins_disable_only_members(#[case] target: Role, #[case] expected: Result<(), AuthzError>) {
        let target = ResourceContext::User(other(target));

        assert_eq!(
            authorize(&actor(Role::Admin), Permission::SetDisabled, &target),
            expected
        );
    }

    #[rstest]
    #[case::owner(Role::Owner)]
    #[case::admin(Role::Admin)]
    #[case::member(Role::Member)]
    fn nobody_changes_their_own_role_or_disables_themselves(#[case] role: Role) {
        let actor = actor(role);
        let me = ResourceContext::User(myself(&actor));

        assert_eq!(
            authorize(&actor, Permission::SetDisabled, &me),
            Err(Forbidden)
        );
        for role in [Role::Member, Role::Admin] {
            let set_role = Permission::SetRole { role };
            assert_eq!(authorize(&actor, set_role, &me), Err(Forbidden));
        }
    }

    #[test]
    fn nobody_becomes_the_owner_through_set_role() {
        let set_role = Permission::SetRole { role: Role::Owner };
        let target = ResourceContext::User(other(Role::Admin));

        assert_eq!(
            authorize(&actor(Role::Owner), set_role, &target),
            Err(Forbidden)
        );
    }

    #[test]
    fn members_cannot_see_other_users_or_invites() {
        let member = actor(Role::Member);
        let user = ResourceContext::User(other(Role::Member));
        let invite = ResourceContext::Invite { role: Role::Member };

        assert_eq!(
            authorize(&member, Permission::SetDisabled, &user),
            Err(NotFound)
        );
        assert_eq!(
            authorize(&member, Permission::RevokeInvite, &invite),
            Err(NotFound)
        );
    }

    // Personal resources.

    #[rstest]
    #[case::owner(Role::Owner)]
    #[case::admin(Role::Admin)]
    #[case::member(Role::Member)]
    fn personal_resources_belong_to_their_owner_only(#[case] role: Role) {
        let actor = actor(role);
        let own = ResourceContext::Personal { owner: actor.id };
        let someone_elses = ResourceContext::Personal {
            owner: user_id(OTHER),
        };

        assert_eq!(authorize(&actor, Permission::UsePersonal, &own), Ok(()));
        assert_eq!(
            authorize(&actor, Permission::UsePersonal, &someone_elses),
            Err(NotFound)
        );
    }
}
