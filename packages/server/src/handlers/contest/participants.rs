use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use broccoli_server_sdk::permissions as perm;
use sea_orm::*;
use tracing::instrument;

// visibility-bypass-audited: `add_participant`/`remove_participant`/
// `bulk_add_participants` require perm::CONTEST_MANAGE, pinned by
// `contestant_cannot_add_participant`, `contestant_cannot_remove_participant`,
// and `contestant_cannot_bulk_add_participants`. `register_for_contest`/
// `unregister_from_contest` are self-service writes on the caller's own
// membership row (window/is_public checked inline), not a third-party read.
// `list_participants` gates contest reachability through
// `check_contest_access` (the same audited pattern as `handlers/contest/mod.rs`,
// pinned by `utils::contest::contest_access_tests`), then requires
// `show_participants_list` or perm::CONTEST_MANAGE - there is no
// `Resource::Participant` in the kernel, so this cannot be routed - pinned by
// `hidden_participant_list_denies_non_manager_participant` and
// `hidden_participant_list_still_readable_by_contest_manager`
// (tests/integration/contest.rs).
use crate::entity::{contest_user, role, user, user_role};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::models::contest::*;
use crate::state::AppState;
use crate::utils::contest::{check_contest_access, find_contest};
use crate::utils::soft_delete::SoftDeletable;

use super::find_contest_for_update;

#[utoipa::path(
    post,
    path = "/",
    tag = "Contest Participants",
    operation_id = "addParticipant",
    summary = "Add a participant to a contest",
    description = "Adds a user to the contest as a participant (admin action). Requires `contest:manage` permission. Returns 409 if the user is already a participant.",
    params(("id" = i32, Path, description = "Contest ID")),
    request_body = AddParticipantRequest,
    responses(
        (status = 201, description = "Participant added", body = ContestParticipantResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest or user not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "User already a participant (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(contest_id))]
pub async fn add_participant(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(contest_id): AppPath<i32>,
    AppJson(payload): AppJson<AddParticipantRequest>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;

    let txn = state.db.begin().await?;
    find_contest_for_update(&txn, contest_id).await?;

    let target_user = user::Entity::find_active_by_id(payload.user_id)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    let now = chrono::Utc::now();
    let new_cu = contest_user::ActiveModel {
        contest_id: Set(contest_id),
        user_id: Set(payload.user_id),
        registered_at: Set(now),
    };

    match new_cu.insert(&txn).await {
        Ok(model) => {
            txn.commit().await?;
            Ok((
                StatusCode::CREATED,
                Json(ContestParticipantResponse {
                    contest_id: model.contest_id,
                    user_id: model.user_id,
                    username: target_user.username,
                    is_deleted: target_user.deleted_at.is_some(),
                    registered_at: model.registered_at,
                }),
            ))
        }
        Err(e) if matches!(e.sql_err(), Some(SqlErr::UniqueConstraintViolation(_))) => {
            Err(AppError::Conflict("Already a participant".into()))
        }
        Err(e) => Err(e.into()),
    }
}
#[utoipa::path(
    get,
    path = "/",
    tag = "Contest Participants",
    operation_id = "listParticipants",
    summary = "List participants of a contest",
    description = "Returns all participants in the contest, ordered by registration time. Requires `contest:manage` permission if `show_participants_list` is false.",
    params(("id" = i32, Path, description = "Contest ID")),
    responses(
        (status = 200, description = "List of participants", body = Vec<ContestParticipantResponse>),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden when show_participants_list is false (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id))]
pub async fn list_participants(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(contest_id): AppPath<i32>,
) -> Result<Json<Vec<ContestParticipantResponse>>, AppError> {
    let contest_model = find_contest(&state.db, contest_id).await?;
    check_contest_access(&state.db, &auth_user, &contest_model).await?;

    if !contest_model.show_participants_list && !auth_user.has_permission(perm::CONTEST_MANAGE) {
        return Err(AppError::PermissionDenied);
    }

    let rows = contest_user::Entity::find()
        .filter(contest_user::Column::ContestId.eq(contest_id))
        .find_also_related(user::Entity)
        .order_by_asc(contest_user::Column::RegisteredAt)
        .all(&state.db)
        .await?;

    let items = rows
        .into_iter()
        .map(|(cu, usr)| ContestParticipantResponse {
            contest_id: cu.contest_id,
            user_id: cu.user_id,
            username: usr.as_ref().map(|u| u.username.clone()).unwrap_or_default(),
            is_deleted: usr.as_ref().is_some_and(|u| u.deleted_at.is_some()),
            registered_at: cu.registered_at,
        })
        .collect();

    Ok(Json(items))
}
#[utoipa::path(
    delete,
    path = "/{user_id}",
    tag = "Contest Participants",
    operation_id = "removeParticipant",
    summary = "Remove a participant from a contest",
    description = "Removes a participant from the contest (admin action). Requires `contest:manage` permission.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("user_id" = i32, Path, description = "User ID"),
    ),
    responses(
        (status = 204, description = "Participant removed"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Participant not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id, user_id))]
pub async fn remove_participant(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, user_id)): AppPath<(i32, i32)>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;

    let txn = state.db.begin().await?;
    find_contest_for_update(&txn, contest_id).await?;
    let cu = contest_user::Entity::find_by_id((contest_id, user_id))
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("Participant not found".into()))?;

    let active: contest_user::ActiveModel = cu.into();
    active.delete(&txn).await?;
    txn.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    post,
    path = "/{id}/register",
    tag = "Contests",
    operation_id = "registerForContest",
    summary = "Self-register for a public contest",
    description = "Registers the authenticated user for an active public contest. Inactive or non-public contests return 404 to prevent enumeration. Blocked after the contest ends. Returns 409 if already registered.",
    params(("id" = i32, Path, description = "Contest ID")),
    responses(
        (status = 201, description = "Registered for contest"),
        (status = 400, description = "Contest has ended (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Already registered (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id))]
pub async fn register_for_contest(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(contest_id): AppPath<i32>,
) -> Result<impl IntoResponse, AppError> {
    let now = chrono::Utc::now();
    let txn = state.db.begin().await?;
    let contest_model = find_contest_for_update(&txn, contest_id).await?;

    if contest_model.activate_time.is_none_or(|at| at > now)
        || contest_model.deactivate_time.is_some_and(|dt| dt <= now)
        || !contest_model.is_public
    {
        return Err(AppError::NotFound("Contest not found".into()));
    }

    if now >= contest_model.end_time {
        return Err(AppError::Validation("Contest has ended".into()));
    }
    let new_cu = contest_user::ActiveModel {
        contest_id: Set(contest_id),
        user_id: Set(auth_user.user_id),
        registered_at: Set(now),
    };

    match new_cu.insert(&txn).await {
        Ok(_) => {
            txn.commit().await?;
            Ok(StatusCode::CREATED)
        }
        Err(e) if matches!(e.sql_err(), Some(SqlErr::UniqueConstraintViolation(_))) => {
            Err(AppError::Conflict("Already registered".into()))
        }
        Err(e) => Err(e.into()),
    }
}
#[utoipa::path(
    delete,
    path = "/{id}/register",
    tag = "Contests",
    operation_id = "unregisterFromContest",
    summary = "Self-unregister from a contest",
    description = "Removes the authenticated user's registration from a contest. Returns 404 if the caller is not registered.",
    params(("id" = i32, Path, description = "Contest ID")),
    responses(
        (status = 204, description = "Unregistered from contest"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Not registered or contest not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id))]
pub async fn unregister_from_contest(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(contest_id): AppPath<i32>,
) -> Result<impl IntoResponse, AppError> {
    let txn = state.db.begin().await?;
    find_contest_for_update(&txn, contest_id).await?;
    let cu = contest_user::Entity::find_by_id((contest_id, auth_user.user_id))
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("Not registered for this contest".into()))?;

    let active: contest_user::ActiveModel = cu.into();
    active.delete(&txn).await?;
    txn.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(
    post,
    path = "/bulk",
    tag = "Contest Participants",
    operation_id = "bulkAddParticipants",
    summary = "Bulk-add participants to a contest",
    description = "Enrolls multiple users in a contest. Existing users are looked up by username; new users can be created with auto-generated or custom passwords. Requires `contest:manage` permission. Partial success model: missing usernames are reported in `not_found`, already-enrolled users in `already_enrolled`.",
    params(("id" = i32, Path, description = "Contest ID")),
    request_body = BulkAddParticipantsRequest,
    responses(
        (status = 200, description = "Participants added", body = BulkAddParticipantsResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(contest_id))]
pub async fn bulk_add_participants(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(contest_id): AppPath<i32>,
    AppJson(payload): AppJson<BulkAddParticipantsRequest>,
) -> Result<Json<BulkAddParticipantsResponse>, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    validate_bulk_add_participants(&payload)?;

    let mut hashed_entries: Vec<(String, String, String)> = Vec::new();
    if !payload.create_users.is_empty() {
        let entries: Vec<(String, String)> = payload
            .create_users
            .iter()
            .map(|e| {
                let username = e.username.trim().to_string();
                let plaintext = e
                    .password
                    .clone()
                    .unwrap_or_else(|| crate::utils::password::generate_password(12));
                (username, plaintext)
            })
            .collect();

        hashed_entries = tokio::task::spawn_blocking(move || {
            entries
                .into_iter()
                .map(|(username, plaintext)| {
                    let hash = crate::utils::hash::hash_password(&plaintext)
                        .map_err(|e| format!("Password hash error for '{username}': {e}"))?;
                    Ok((username, plaintext, hash))
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .await
        .map_err(|e| AppError::Internal(format!("Password hashing task failed: {e}")))?
        .map_err(AppError::Internal)?;
    }

    let txn = state.db.begin().await?;
    find_contest_for_update(&txn, contest_id).await?;

    let mut added = Vec::new();
    let mut created_response = Vec::new();
    let mut already_enrolled = Vec::new();
    let mut not_found = Vec::new();

    let mut users_to_enroll: Vec<(i32, String)> = Vec::new();

    let existing_created_user_map = if hashed_entries.is_empty() {
        std::collections::HashMap::new()
    } else {
        let requested_usernames: Vec<String> = hashed_entries
            .iter()
            .map(|(username, _, _)| username.clone())
            .collect();
        user::Entity::find_active()
            .filter(user::Column::Username.is_in(requested_usernames))
            .all(&txn)
            .await?
            .into_iter()
            .map(|user| (user.username.clone(), user.id))
            .collect::<std::collections::HashMap<_, _>>()
    };

    for (username, plaintext, hash) in hashed_entries {
        if let Some(&existing_user_id) = existing_created_user_map.get(&username) {
            users_to_enroll.push((existing_user_id, username));
            continue;
        }

        let new_user = user::ActiveModel {
            username: Set(username.clone()),
            password: Set(hash),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };

        match new_user.insert(&txn).await {
            Ok(user) => {
                for role_name in role::DEFAULT_ROLES {
                    let role = role::Entity::find_by_id(role_name.to_string())
                        .one(&txn)
                        .await?
                        .ok_or_else(|| {
                            AppError::Internal(format!("Default role '{}' not found", role_name))
                        })?;

                    user_role::ActiveModel {
                        user_id: Set(user.id),
                        role: Set(role.name),
                    }
                    .insert(&txn)
                    .await?;
                }

                created_response.push(BulkParticipantCreated {
                    user_id: user.id,
                    username: username.clone(),
                    password: plaintext,
                });
                users_to_enroll.push((user.id, username));
            }
            Err(e) if matches!(e.sql_err(), Some(SqlErr::UniqueConstraintViolation(_))) => {
                return Err(AppError::Validation(format!(
                    "User '{username}' was created concurrently; retry the bulk participant request"
                )));
            }
            Err(e) => return Err(e.into()),
        }
    }

    if !payload.usernames.is_empty() {
        let trimmed_usernames: Vec<String> = payload
            .usernames
            .iter()
            .map(|u| u.trim().to_string())
            .collect();

        let found_users: Vec<user::Model> = user::Entity::find_active()
            .filter(user::Column::Username.is_in(trimmed_usernames.clone()))
            .all(&txn)
            .await?;

        let found_map: std::collections::HashMap<String, i32> = found_users
            .iter()
            .map(|u| (u.username.clone(), u.id))
            .collect();

        for name in &trimmed_usernames {
            if let Some(&uid) = found_map.get(name) {
                users_to_enroll.push((uid, name.clone()));
            } else {
                not_found.push(name.clone());
            }
        }
    }

    // A user can be referenced more than once (the same username listed twice,
    // or a freshly-created user also named in `usernames`). Dedupe by user id
    // so each user is enrolled at most once: a second insert of the same
    // (contest_id, user_id) would raise a unique violation which, caught inside
    // this transaction, would abort the whole batch (see the enrol loop below).
    let mut seen_enroll_uids = std::collections::HashSet::new();
    users_to_enroll.retain(|(uid, _)| seen_enroll_uids.insert(*uid));

    let user_ids_to_check: Vec<i32> = users_to_enroll.iter().map(|(id, _)| *id).collect();
    let already_enrolled_ids: std::collections::HashSet<i32> = if !user_ids_to_check.is_empty() {
        contest_user::Entity::find()
            .filter(contest_user::Column::ContestId.eq(contest_id))
            .filter(contest_user::Column::UserId.is_in(user_ids_to_check))
            .select_only()
            .column(contest_user::Column::UserId)
            .into_tuple::<i32>()
            .all(&txn)
            .await?
            .into_iter()
            .collect()
    } else {
        std::collections::HashSet::new()
    };

    let now = chrono::Utc::now();
    let created_user_ids: std::collections::HashSet<i32> =
        created_response.iter().map(|c| c.user_id).collect();

    for (uid, name) in users_to_enroll {
        if already_enrolled_ids.contains(&uid) {
            if !created_user_ids.contains(&uid) {
                already_enrolled.push(BulkParticipantAdded {
                    user_id: uid,
                    username: name,
                });
            }
            continue;
        }

        let new_cu = contest_user::ActiveModel {
            contest_id: Set(contest_id),
            user_id: Set(uid),
            registered_at: Set(now),
        };
        // ON CONFLICT DO NOTHING instead of catching a UniqueConstraintViolation:
        // a caught constraint error aborts the surrounding transaction (Postgres
        // has no implicit savepoint), so one already-enrolled user would roll the
        // whole batch back and enrol nobody. A conflict here instead reports 0
        // rows affected, which we classify as "already enrolled" (a user enrolled
        // concurrently, despite the pre-check under the contest lock).
        let inserted = contest_user::Entity::insert(new_cu)
            .on_conflict(
                sea_orm::sea_query::OnConflict::columns([
                    contest_user::Column::ContestId,
                    contest_user::Column::UserId,
                ])
                .do_nothing()
                .to_owned(),
            )
            .exec_without_returning(&txn)
            .await?;

        if created_user_ids.contains(&uid) {
            continue;
        }
        if inserted == 0 {
            already_enrolled.push(BulkParticipantAdded {
                user_id: uid,
                username: name,
            });
        } else {
            added.push(BulkParticipantAdded {
                user_id: uid,
                username: name,
            });
        }
    }

    txn.commit().await?;

    tracing::info!(
        contest_id,
        added = added.len(),
        created = created_response.len(),
        already_enrolled = already_enrolled.len(),
        not_found = not_found.len(),
        user_id = auth_user.user_id,
        "Bulk added participants"
    );

    Ok(Json(BulkAddParticipantsResponse {
        added,
        created: created_response,
        already_enrolled,
        not_found,
    }))
}
