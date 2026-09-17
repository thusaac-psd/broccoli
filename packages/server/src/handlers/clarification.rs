use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
use sea_orm::*;
use std::collections::{HashMap, HashSet};
use tracing::instrument;

// visibility-bypass-audited: `list_clarifications`/`create_clarification`/
// `reply_clarification`/`resolve_clarification` all route contest
// reachability through `VisibilityKernel` (`Resource::Contest` below)
// BEFORE any row lookup, and `list_clarifications`'s per-row visibility is
// the kernel's `fetch_visible_batch`
// (`visibility::host_rules::decide_clarification`) - see the comments at
// those call sites. `reply_clarification`/`toggle_reply_public`/
// `resolve_clarification` are write paths on a single clarification the
// caller must already be the admin, author, or recipient of (asserted
// inline, since "is a party to this thread" has no `Resource::Clarification`
// read-decision equivalent) - the contest-reachability gate only prevents
// an unreachable contest's existing-vs-missing clarification id from being
// distinguishable via 403-vs-404; it does not replace the inline
// admin/author/recipient check. Every response they return reflects only
// that one clarification the caller was just authorized to act on - the
// same write-reflects-own-result pattern as `handlers/submission/rejudge.rs`.
// `user` here is only used by `resolve_usernames`, a post-authorization
// display helper. Pinned by the frozen
// `tests/integration/visibility_matrix.rs::contest_clarification_list` suite
// and `tests/integration/clarification.rs::clarification_actions`.
use crate::entity::{clarification, clarification_reply, user};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::models::clarification::*;
use crate::state::AppState;
use crate::utils::text::sanitize_db_text;
use crate::visibility::{Action, Resource, Subject, VisibilityKernel};

async fn resolve_usernames(
    db: &DatabaseConnection,
    user_ids: &HashSet<i32>,
) -> Result<HashMap<i32, String>, AppError> {
    if user_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let users: Vec<user::Model> = user::Entity::find()
        .filter(user::Column::Id.is_in(user_ids.iter().copied()))
        .all(db)
        .await?;

    Ok(users.into_iter().map(|u| (u.id, u.username)).collect())
}

#[utoipa::path(
    get,
    path = "/",
    tag = "Clarifications",
    operation_id = "listClarifications",
    summary = "List clarifications for a contest",
    description = "Returns clarifications visible to the current user. Users with `contest:manage` see all; \
                   others see their own questions, public announcements, public replies, \
                   and direct messages addressed to them.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ClarificationListQuery,
    ),
    responses(
        (status = 200, description = "List of clarifications", body = ClarificationListResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, query))]
pub async fn list_clarifications(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(contest_id): AppPath<i32>,
    Query(query): Query<ClarificationListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));

    // Fail fast, before reading any clarification rows below, on a contest
    // the kernel already knows is unreachable. Folds in what `find_contest`
    // + `check_contest_access` used to check here (existence, the
    // activation window, `is_public`, `contest_user` membership) - see
    // `visibility::host_rules::decide_contest`. Both of that pair's `Err`
    // branches already returned exactly this same `AppError::NotFound`
    // message, so this is not a behavioural change.
    //
    // NOTE: this is a pure reachability gate - `is_denied()` treats a
    // hypothetical `Redact` on this `Resource::Contest` decision the same
    // as `Allow`, and the contest itself is never rendered from this
    // decision (only used to decide whether to proceed), so `Redact`
    // degenerates to `Allow` here. No plugin currently returns `Redact` for
    // `Resource::Contest`, so this is a documented no-op today, not a live
    // bug - see Task 20 Item 6.
    if kernel
        .decide(Action::Read, Resource::Contest(contest_id))
        .await?
        .is_denied()
    {
        return Err(AppError::NotFound("Contest not found".into()));
    }

    let is_admin = auth_user.has_permission(perm::CONTEST_MANAGE);

    let mut select =
        clarification::Entity::find().filter(clarification::Column::ContestId.eq(contest_id));

    if let Some(ref type_filter) = query.r#type {
        select = select.filter(clarification::Column::ClarificationType.eq(type_filter.as_str()));
    }

    // The author/is_public/recipient prefilter that used to live here is now
    // the kernel's job (`visibility::host_rules::decide_clarification`,
    // rule 10): every row for this contest (matching the optional type
    // filter) is fetched, and a row this viewer may not see comes back
    // `Deny` from `fetch_visible_batch` below and is dropped from `data`,
    // never rendered as a placeholder.
    let rows = select
        .order_by_desc(clarification::Column::CreatedAt)
        .all(&state.db)
        .await?;

    let clarification_ids: Vec<i32> = rows.iter().map(|r| r.id).collect();

    let all_replies = if clarification_ids.is_empty() {
        vec![]
    } else {
        clarification_reply::Entity::find()
            .filter(clarification_reply::Column::ClarificationId.is_in(clarification_ids))
            .order_by_asc(clarification_reply::Column::CreatedAt)
            .all(&state.db)
            .await?
    };

    let mut replies_map: HashMap<i32, Vec<clarification_reply::Model>> = HashMap::new();
    let mut user_ids = HashSet::new();

    for reply in all_replies {
        user_ids.insert(reply.author_id);
        replies_map
            .entry(reply.clarification_id)
            .or_default()
            .push(reply);
    }

    for r in &rows {
        user_ids.insert(r.author_id);
        if let Some(rid) = r.recipient_id {
            user_ids.insert(rid);
        }
        if let Some(raid) = r.reply_author_id {
            user_ids.insert(raid);
        }
        if let Some(rb) = r.resolved_by {
            user_ids.insert(rb);
        }
    }

    let user_map = resolve_usernames(&state.db, &user_ids).await?;

    let items: Vec<(Resource, ClarificationResponse)> = rows
        .into_iter()
        .map(|r| {
            let author_name = user_map
                .get(&r.author_id)
                .cloned()
                .unwrap_or_else(|| "[Deleted]".into());
            let recipient_name = r.recipient_id.and_then(|rid| user_map.get(&rid).cloned());
            let reply_author_name = r
                .reply_author_id
                .and_then(|raid| user_map.get(&raid).cloned());
            let resolved_by_name = r.resolved_by.and_then(|uid| user_map.get(&uid).cloned());

            // Still needed for the `replies` array's own per-element filter
            // below (`clarification.rs:157-171`'s original predicate, kept
            // verbatim) - a `FieldMask` can blank a field uniformly across
            // every array element but cannot omit only SOME elements by a
            // per-element predicate, so that part stays handler-side (see
            // `visibility::host_rules`'s module docs, "Deliberately not
            // ported"). Whether THIS ROW is shown at all, and whether its own
            // legacy `reply_*` fields are redacted, is now entirely the
            // kernel's job via `Resource::Clarification` below - the
            // `show_question`/`show_reply` locals this used to compute for
            // that are gone.
            let is_participant = is_admin
                || r.author_id == auth_user.user_id
                || r.recipient_id == Some(auth_user.user_id);

            let all_replies = replies_map.remove(&r.id).unwrap_or_default();
            let replies = all_replies
                .into_iter()
                .filter(|rep| is_admin || rep.is_public || is_participant)
                .map(|rep| ClarificationReplyResponse {
                    id: rep.id,
                    author_id: rep.author_id,
                    author_name: user_map
                        .get(&rep.author_id)
                        .cloned()
                        .unwrap_or_else(|| "[Deleted]".into()),
                    content: rep.content,
                    is_public: rep.is_public,
                    created_at: rep.created_at,
                })
                .collect();

            let resource = Resource::Clarification(r.id);
            let dto = ClarificationResponse {
                id: r.id,
                contest_id: r.contest_id,
                author_id: r.author_id,
                author_name,
                content: r.content,
                clarification_type: r.clarification_type,
                recipient_id: r.recipient_id,
                recipient_name,
                is_public: r.is_public,
                reply_content: r.reply_content,
                reply_author_id: r.reply_author_id,
                reply_author_name,
                reply_is_public: r.reply_is_public,
                replied_at: r.replied_at,
                replies,
                resolved: r.resolved,
                resolved_at: r.resolved_at,
                resolved_by: r.resolved_by,
                resolved_by_name,
                created_at: r.created_at,
                updated_at: r.updated_at,
            };
            (resource, dto)
        })
        .collect();

    // Per-row kernel decision: a denied row (kernel `Deny`, e.g. a private
    // question this viewer is neither the author, the recipient, nor
    // `contest:manage` for) is omitted from `data` outright via `.flatten()`
    // - never rendered as a placeholder, which would itself confirm the row
    // exists. A `Redact` decision blanks exactly the legacy `reply_content`/
    // `reply_author_id`/`reply_author_name`/`replied_at` fields once
    // serialized - see `visibility::host_rules::decide_clarification`.
    let visible = kernel.fetch_visible_batch(Action::Read, items).await?;
    let data: Vec<serde_json::Value> = visible
        .into_iter()
        .flatten()
        .map(|v| v.into_masked_json())
        .collect::<Result<_, _>>()?;

    Ok(Json(serde_json::json!({ "data": data })))
}

#[utoipa::path(
    post,
    path = "/",
    tag = "Clarifications",
    operation_id = "createClarification",
    summary = "Create a clarification",
    description = "Users can create questions. Users with `contest:manage` permission can also create announcements and direct messages to specific participants.",
    params(("id" = i32, Path, description = "Contest ID")),
    request_body = CreateClarificationRequest,
    responses(
        (status = 201, description = "Clarification created", body = ClarificationResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest or recipient not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload))]
pub async fn create_clarification(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(contest_id): AppPath<i32>,
    AppJson(payload): AppJson<CreateClarificationRequest>,
) -> Result<impl IntoResponse, AppError> {
    validate_create_clarification(&payload)?;

    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));

    // Fail fast, before any of the business-rule checks below, on a contest
    // the kernel already knows is unreachable. There is no clarification row
    // yet to decide about at POST time, only "can this subject reach this
    // contest at all" - so this reuses `Resource::Contest`/rules 1-4
    // verbatim via `Action::Clarify` (`host_decide` does not branch on
    // `Action` for `Resource::Contest`, so this is byte-identical to the
    // `Action::Read` gate above it in `list_clarifications`). Both of the
    // `find_contest` + `check_contest_access` pair's `Err` branches already
    // returned exactly this same `AppError::NotFound` message, so this is
    // not a behavioural change. See `visibility::host_rules`'s module docs
    // for why this needs no new host-rule code.
    //
    // NOTE: as in `list_clarifications` above, this is a pure reachability
    // gate - `is_denied()` treats a hypothetical `Redact` here the same as
    // `Allow`, and this decision's `Resource::Contest` is never rendered
    // from here, so `Redact` degenerates to `Allow`. No plugin currently
    // returns `Redact` for `Resource::Contest`, so this is a documented
    // no-op today, not a live bug - see Task 20 Item 6.
    if kernel
        .decide(Action::Clarify, Resource::Contest(contest_id))
        .await?
        .is_denied()
    {
        return Err(AppError::NotFound("Contest not found".into()));
    }

    let is_admin = auth_user.has_permission(perm::CONTEST_MANAGE);

    if !is_admin && payload.clarification_type != "question" {
        return Err(AppError::PermissionDenied);
    }

    // Only admins may direct a clarification to a specific recipient. Honoring a
    // caller-supplied recipient_id for a non-admin would let a contestant inject
    // a privately-visible clarification into any chosen user's view.
    let recipient_id = if is_admin { payload.recipient_id } else { None };

    let mut recipient_name = None;
    if let Some(recipient_id) = recipient_id {
        let recipient = user::Entity::find_by_id(recipient_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| AppError::NotFound("Recipient user not found".into()))?;
        recipient_name = Some(recipient.username);
    }

    // Only admins may publish a clarification to ALL participants. A non-admin's
    // supplied is_public is ignored (forced false), the same way recipient_id and
    // the announcement type are gated above - otherwise a contestant could set
    // is_public:true on their own "question" and broadcast arbitrary content to
    // every participant, an unmoderated cross-contestant channel.
    let is_public = if payload.clarification_type == "announcement" {
        true
    } else {
        is_admin && payload.is_public.unwrap_or(false)
    };

    let now = chrono::Utc::now();
    let new = clarification::ActiveModel {
        contest_id: Set(contest_id),
        author_id: Set(auth_user.user_id),
        content: Set(sanitize_db_text(payload.content.trim())),
        clarification_type: Set(payload.clarification_type.clone()),
        recipient_id: Set(recipient_id),
        is_public: Set(is_public),
        reply_is_public: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    let model = new.insert(&state.db).await?;

    let resp = ClarificationResponse {
        id: model.id,
        contest_id: model.contest_id,
        author_id: model.author_id,
        author_name: auth_user.username.clone(),
        content: model.content,
        clarification_type: model.clarification_type,
        recipient_id: model.recipient_id,
        recipient_name,
        is_public: model.is_public,
        reply_content: model.reply_content,
        reply_author_id: model.reply_author_id,
        reply_author_name: None,
        reply_is_public: model.reply_is_public,
        replied_at: model.replied_at,
        replies: vec![],
        resolved: model.resolved,
        resolved_at: model.resolved_at,
        resolved_by: model.resolved_by,
        resolved_by_name: None,
        created_at: model.created_at,
        updated_at: model.updated_at,
    };

    Ok((StatusCode::CREATED, Json(resp)))
}

#[utoipa::path(
    post,
    path = "/{clarification_id}/reply",
    tag = "Clarifications",
    operation_id = "replyClarification",
    summary = "Reply to a clarification",
    description = "Users with `contest:manage` permission, the question author, or the DM recipient can reply. \
                   Multiple replies are allowed. When `is_public` is true, the reply becomes visible to all participants.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("clarification_id" = i32, Path, description = "Clarification ID"),
    ),
    request_body = ReplyClarificationRequest,
    responses(
        (status = 200, description = "Reply saved", body = ClarificationResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Clarification not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload))]
pub async fn reply_clarification(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, clarification_id)): AppPath<(i32, i32)>,
    AppJson(payload): AppJson<ReplyClarificationRequest>,
) -> Result<Json<ClarificationResponse>, AppError> {
    validate_reply_clarification(&payload)?;

    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));

    // Fail fast, before the row lookup below, on a contest the kernel
    // already knows is unreachable - the same `Resource::Contest`/
    // `Action::Clarify` gate `create_clarification` uses. Without this, a
    // stranger to a private or inactive contest could distinguish an
    // existing clarification id (403 PermissionDenied, since the row exists
    // but they're neither admin, author, nor recipient) from a non-existing
    // one (404 NotFound) - confirming the row exists without ever being
    // authorized to see it. Gating reachability first collapses both cases
    // to the same 404, matching `list_clarifications`/`create_clarification`.
    if kernel
        .decide(Action::Clarify, Resource::Contest(contest_id))
        .await?
        .is_denied()
    {
        return Err(AppError::NotFound("Contest not found".into()));
    }

    let existing = clarification::Entity::find_by_id(clarification_id)
        .filter(clarification::Column::ContestId.eq(contest_id))
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Clarification not found".into()))?;

    let is_admin = auth_user.has_permission(perm::CONTEST_MANAGE);
    let is_author = existing.author_id == auth_user.user_id;
    let is_recipient = existing.recipient_id == Some(auth_user.user_id);

    if !is_admin && !is_author && !is_recipient {
        return Err(AppError::PermissionDenied);
    }

    let now = chrono::Utc::now();
    let txn = state.db.begin().await?;

    // Only admins may publish a reply to ALL participants - the same gate the
    // create path and toggle_reply_public enforce. A non-admin author/recipient
    // may reply, but their supplied `is_public` is forced false; otherwise they
    // could broadcast arbitrary content to every participant. Forcing it here (as
    // the create path does) is why the documented `is_public` field had no effect:
    // replies were pinned private and an admin had to make a second toggle call.
    let reply_public = is_admin && payload.is_public;

    let new_reply = clarification_reply::ActiveModel {
        clarification_id: Set(clarification_id),
        author_id: Set(auth_user.user_id),
        content: Set(sanitize_db_text(payload.content.trim())),
        is_public: Set(reply_public),
        created_at: Set(now),
        ..Default::default()
    };
    new_reply.insert(&txn).await?;

    // The parent `reply_is_public` is the "ANY reply is public" aggregate
    // (see toggle_reply_public), not the latest reply's own flag. Recompute it
    // across every reply - including the one just inserted - instead of clobbering
    // it to false, which would drop an already-public earlier reply's aggregate.
    let any_reply_public = clarification_reply::Entity::find()
        .filter(clarification_reply::Column::ClarificationId.eq(clarification_id))
        .filter(clarification_reply::Column::IsPublic.eq(true))
        .count(&txn)
        .await?
        > 0;

    let mut active: clarification::ActiveModel = existing.into();
    active.reply_content = Set(Some(sanitize_db_text(payload.content.trim())));
    active.reply_author_id = Set(Some(auth_user.user_id));
    active.reply_is_public = Set(any_reply_public);
    active.replied_at = Set(Some(now));
    active.updated_at = Set(now);
    let model = active.update(&txn).await?;

    txn.commit().await?;

    let reply_rows = clarification_reply::Entity::find()
        .filter(clarification_reply::Column::ClarificationId.eq(clarification_id))
        .order_by_asc(clarification_reply::Column::CreatedAt)
        .all(&state.db)
        .await?;

    let mut user_ids = HashSet::new();
    user_ids.insert(model.author_id);
    if let Some(rid) = model.recipient_id {
        user_ids.insert(rid);
    }
    if let Some(raid) = model.reply_author_id {
        user_ids.insert(raid);
    }
    if let Some(rb) = model.resolved_by {
        user_ids.insert(rb);
    }
    for rep in &reply_rows {
        user_ids.insert(rep.author_id);
    }

    let user_map = resolve_usernames(&state.db, &user_ids).await?;

    let author_name = user_map
        .get(&model.author_id)
        .cloned()
        .unwrap_or_else(|| "[Deleted]".into());
    let recipient_name = model
        .recipient_id
        .and_then(|rid| user_map.get(&rid).cloned());
    let reply_author_name = model
        .reply_author_id
        .and_then(|raid| user_map.get(&raid).cloned());
    let resolved_by_name = model
        .resolved_by
        .and_then(|uid| user_map.get(&uid).cloned());

    let replies = reply_rows
        .into_iter()
        .map(|rep| ClarificationReplyResponse {
            id: rep.id,
            author_id: rep.author_id,
            author_name: user_map
                .get(&rep.author_id)
                .cloned()
                .unwrap_or_else(|| "[Deleted]".into()),
            content: rep.content,
            is_public: rep.is_public,
            created_at: rep.created_at,
        })
        .collect();

    Ok(Json(ClarificationResponse {
        id: model.id,
        contest_id: model.contest_id,
        author_id: model.author_id,
        author_name,
        content: model.content,
        clarification_type: model.clarification_type,
        recipient_id: model.recipient_id,
        recipient_name,
        is_public: model.is_public,
        reply_content: model.reply_content,
        reply_author_id: model.reply_author_id,
        reply_author_name,
        reply_is_public: model.reply_is_public,
        replied_at: model.replied_at,
        replies,
        resolved: model.resolved,
        resolved_at: model.resolved_at,
        resolved_by: model.resolved_by,
        resolved_by_name,
        created_at: model.created_at,
        updated_at: model.updated_at,
    }))
}

#[utoipa::path(
    post,
    path = "/{clarification_id}/replies/{reply_id}/toggle-public",
    tag = "Clarifications",
    operation_id = "toggleReplyPublic",
    summary = "Toggle a reply's public visibility",
    description = "Requires `contest:manage` permission. Promotes a private reply to a public announcement or reverts it. Optionally makes the parent question public as well.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("clarification_id" = i32, Path, description = "Clarification ID"),
        ("reply_id" = i32, Path, description = "Reply ID"),
        ToggleReplyPublicQuery,
    ),
    responses(
        (status = 200, description = "Reply visibility toggled", body = ClarificationReplyResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Reply or Clarification not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, query))]
pub async fn toggle_reply_public(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, clarification_id, reply_id)): AppPath<(i32, i32, i32)>,
    Query(query): Query<ToggleReplyPublicQuery>,
) -> Result<Json<ClarificationReplyResponse>, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;

    let txn = state.db.begin().await?;

    let parent = clarification::Entity::find_by_id(clarification_id)
        .filter(clarification::Column::ContestId.eq(contest_id))
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("Clarification not found in this contest".into()))?;

    let reply = clarification_reply::Entity::find_by_id(reply_id)
        .filter(clarification_reply::Column::ClarificationId.eq(clarification_id))
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("Reply not found".into()))?;

    let new_is_public = !reply.is_public;
    let mut active: clarification_reply::ActiveModel = reply.into();
    active.is_public = Set(new_is_public);
    let updated = active.update(&txn).await?;

    let any_public = clarification_reply::Entity::find()
        .filter(clarification_reply::Column::ClarificationId.eq(clarification_id))
        .filter(clarification_reply::Column::IsPublic.eq(true))
        .count(&txn)
        .await?
        > 0;

    let mut parent_active: clarification::ActiveModel = parent.into();
    parent_active.reply_is_public = Set(any_public);

    if new_is_public && query.include_question.unwrap_or(false) {
        parent_active.is_public = Set(true);
    }
    parent_active.update(&txn).await?;

    txn.commit().await?;

    let author_name = user::Entity::find_by_id(updated.author_id)
        .one(&state.db)
        .await?
        .map(|u| u.username)
        .unwrap_or_else(|| "[Deleted]".into());

    Ok(Json(ClarificationReplyResponse {
        id: updated.id,
        author_id: updated.author_id,
        author_name,
        content: updated.content,
        is_public: updated.is_public,
        created_at: updated.created_at,
    }))
}

#[utoipa::path(
    post,
    path = "/{clarification_id}/resolve",
    tag = "Clarifications",
    operation_id = "resolveClarification",
    summary = "Resolve or reopen a clarification thread",
    description = "Users with `contest:manage` permission or the question author can mark a thread as resolved or reopen it.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("clarification_id" = i32, Path, description = "Clarification ID"),
    ),
    request_body = ResolveClarificationRequest,
    responses(
        (status = 200, description = "Status updated", body = ClarificationResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Clarification not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload))]
pub async fn resolve_clarification(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, clarification_id)): AppPath<(i32, i32)>,
    AppJson(payload): AppJson<ResolveClarificationRequest>,
) -> Result<Json<ClarificationResponse>, AppError> {
    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth_user));

    // Fail fast, before the row lookup below, on a contest the kernel
    // already knows is unreachable - same rationale and gate as
    // `reply_clarification` above: without this, a stranger to a private
    // or inactive contest could distinguish an existing clarification id
    // (403 PermissionDenied) from a non-existing one (404 NotFound).
    if kernel
        .decide(Action::Clarify, Resource::Contest(contest_id))
        .await?
        .is_denied()
    {
        return Err(AppError::NotFound("Contest not found".into()));
    }

    let existing = clarification::Entity::find_by_id(clarification_id)
        .filter(clarification::Column::ContestId.eq(contest_id))
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Clarification not found".into()))?;

    let is_admin = auth_user.has_permission(perm::CONTEST_MANAGE);
    let is_author = existing.author_id == auth_user.user_id;

    if !is_admin && !is_author {
        return Err(AppError::PermissionDenied);
    }

    let now = chrono::Utc::now();
    let mut active: clarification::ActiveModel = existing.clone().into();
    active.resolved = Set(payload.resolved);
    active.resolved_at = Set(if payload.resolved { Some(now) } else { None });
    active.resolved_by = Set(if payload.resolved {
        Some(auth_user.user_id)
    } else {
        None
    });
    active.updated_at = Set(now);
    let model = active.update(&state.db).await?;

    let reply_rows = clarification_reply::Entity::find()
        .filter(clarification_reply::Column::ClarificationId.eq(clarification_id))
        .order_by_asc(clarification_reply::Column::CreatedAt)
        .all(&state.db)
        .await?;

    let mut user_ids = HashSet::new();
    user_ids.insert(model.author_id);
    if let Some(rid) = model.recipient_id {
        user_ids.insert(rid);
    }
    if let Some(raid) = model.reply_author_id {
        user_ids.insert(raid);
    }
    if let Some(rb) = model.resolved_by {
        user_ids.insert(rb);
    }
    for rep in &reply_rows {
        user_ids.insert(rep.author_id);
    }

    let user_map = resolve_usernames(&state.db, &user_ids).await?;

    let author_name = user_map
        .get(&model.author_id)
        .cloned()
        .unwrap_or_else(|| "[Deleted]".into());
    let recipient_name = model
        .recipient_id
        .and_then(|rid| user_map.get(&rid).cloned());
    let reply_author_name = model
        .reply_author_id
        .and_then(|raid| user_map.get(&raid).cloned());
    let resolved_by_name = model
        .resolved_by
        .and_then(|uid| user_map.get(&uid).cloned());

    let replies = reply_rows
        .into_iter()
        .map(|rep| ClarificationReplyResponse {
            id: rep.id,
            author_id: rep.author_id,
            author_name: user_map
                .get(&rep.author_id)
                .cloned()
                .unwrap_or_else(|| "[Deleted]".into()),
            content: rep.content,
            is_public: rep.is_public,
            created_at: rep.created_at,
        })
        .collect();

    Ok(Json(ClarificationResponse {
        id: model.id,
        contest_id: model.contest_id,
        author_id: model.author_id,
        author_name,
        content: model.content,
        clarification_type: model.clarification_type,
        recipient_id: model.recipient_id,
        recipient_name,
        is_public: model.is_public,
        reply_content: model.reply_content,
        reply_author_id: model.reply_author_id,
        reply_author_name,
        reply_is_public: model.reply_is_public,
        replied_at: model.replied_at,
        replies,
        resolved: model.resolved,
        resolved_at: model.resolved_at,
        resolved_by: model.resolved_by,
        resolved_by_name,
        created_at: model.created_at,
        updated_at: model.updated_at,
    }))
}
