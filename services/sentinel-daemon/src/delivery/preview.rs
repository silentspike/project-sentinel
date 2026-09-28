use super::{
    canonical_release_reference, AuthorityRole, ContentDigest, DeliveryAggregateV1, DeliveryError,
    DeliveryState, PreviewAccessV1, PrincipalV1, ReleaseManifestV1, ReleaseState, VersionedRefV1,
    DELIVERY_PREVIEW_MAX_TTL_MS, DELIVERY_PREVIEW_TTL_POLICY_V1, DELIVERY_SCHEMA_V1,
};

pub(super) fn preview_target<'a>(
    aggregate: &'a DeliveryAggregateV1,
    caller: &PrincipalV1,
    delivery: &VersionedRefV1,
    release: &VersionedRefV1,
) -> Result<&'a ReleaseManifestV1, DeliveryError> {
    let denied = || DeliveryError::AuthorityDenied("preview binding is unavailable".to_string());
    let receipt = aggregate.deliveries.get(&delivery.id).ok_or_else(denied)?;
    let target = aggregate.releases.get(&release.id).ok_or_else(denied)?;
    let manifest = aggregate
        .manifests
        .get(&target.manifest.id)
        .ok_or_else(denied)?;
    let mut sealed_receipt = receipt.clone();
    sealed_receipt.state = DeliveryState::Delivered;
    if aggregate.schema_version != DELIVERY_SCHEMA_V1
        || aggregate.tenant_id != caller.tenant_id
        || !caller.has_role(AuthorityRole::Customer)
        || caller.authority_generation == 0
        || receipt.schema_version != DELIVERY_SCHEMA_V1
        || receipt.tenant_id != caller.tenant_id
        || receipt.customer_principal_id != caller.principal_id
        || receipt.delivery_id != delivery.id
        || receipt.generation != delivery.generation
        || receipt.receipt_digest != delivery.digest
        || receipt.receipt_digest != sealed_receipt.computed_digest()?
        || receipt.release != *release
        || !matches!(
            receipt.state,
            DeliveryState::Delivered | DeliveryState::Accepted
        )
        || target.schema_version != DELIVERY_SCHEMA_V1
        || canonical_release_reference(target)? != *release
        || target.state != ReleaseState::Active
        || aggregate.active_release_id.as_deref() != Some(target.release_id.as_str())
        || manifest.schema_version != DELIVERY_SCHEMA_V1
        || manifest.tenant_id != caller.tenant_id
        || manifest.project.id != aggregate.project_id
        || manifest.manifest_id != target.manifest.id
        || manifest.generation != target.manifest.generation
        || manifest.manifest_digest != target.manifest.digest
        || manifest.manifest_digest != manifest.computed_digest()?
        || manifest.artifacts.is_empty()
        || manifest.artifacts.len() > 64
        || ContentDigest::of_domain("m0-preview", DELIVERY_SCHEMA_V1, &manifest.source_digest)?
            != receipt.preview_digest
    {
        return Err(denied());
    }
    Ok(manifest)
}

// Expired/retired grants remain valid history, but never current read authority.
pub(super) fn validate_preview_access(
    aggregate: &DeliveryAggregateV1,
    access: &PreviewAccessV1,
) -> Result<(), DeliveryError> {
    let corrupt = || DeliveryError::CorruptStore("preview access binding is invalid".to_string());
    let receipt = aggregate
        .deliveries
        .get(&access.delivery.id)
        .ok_or_else(corrupt)?;
    let release = aggregate
        .releases
        .get(&access.release.id)
        .ok_or_else(corrupt)?;
    let manifest = aggregate
        .manifests
        .get(&access.manifest.id)
        .ok_or_else(corrupt)?;
    if access.schema_version != DELIVERY_SCHEMA_V1
        || access.access_id.is_empty()
        || access.access_id.len() > 512
        || access.generation == 0
        || access.generation > aggregate.revision
        || access.tenant_id != aggregate.tenant_id
        || access.project_id != aggregate.project_id
        || access.customer.tenant_id != access.tenant_id
        || !access.customer.has_role(AuthorityRole::Customer)
        || access.customer.authority_generation == 0
        || access.customer.principal_id != receipt.customer_principal_id
        || access.delivery.generation != receipt.generation
        || access.delivery.digest != receipt.receipt_digest
        || access.release != receipt.release
        || canonical_release_reference(release)? != access.release
        || access.manifest != release.manifest
        || access.manifest.generation != manifest.generation
        || access.manifest.digest != manifest.manifest_digest
        || access.preview_digest != receipt.preview_digest
        || access.preview_ttl_policy_version != DELIVERY_PREVIEW_TTL_POLICY_V1
        || access.issued_at_ms < receipt.issued_at_ms
        || access.expires_at_ms <= access.issued_at_ms
        || access.expires_at_ms - access.issued_at_ms > DELIVERY_PREVIEW_MAX_TTL_MS
        || access.access_digest != access.computed_digest()?
    {
        return Err(corrupt());
    }
    Ok(())
}

pub fn authorize_delivery_preview<'a>(
    aggregate: &'a DeliveryAggregateV1,
    caller: &PrincipalV1,
    delivery: &VersionedRefV1,
    release: &VersionedRefV1,
    access_ref: Option<&VersionedRefV1>,
    now_ms: u64,
) -> Result<&'a ReleaseManifestV1, DeliveryError> {
    let manifest = preview_target(aggregate, caller, delivery, release)?;
    let receipt = &aggregate.deliveries[&delivery.id];
    let denied =
        || DeliveryError::AuthorityDenied("preview access is expired or revoked".to_string());
    let (issued, expires, policy) = if let Some(reference) = access_ref {
        let access = aggregate
            .preview_access
            .get(&delivery.id)
            .ok_or_else(denied)?;
        validate_preview_access(aggregate, access)?;
        if access.reference() != *reference || access.customer != *caller {
            return Err(denied());
        }
        (
            access.issued_at_ms,
            access.expires_at_ms,
            access.preview_ttl_policy_version,
        )
    } else {
        (
            receipt.issued_at_ms,
            receipt.expires_at_ms,
            receipt.preview_ttl_policy_version,
        )
    };
    if policy != DELIVERY_PREVIEW_TTL_POLICY_V1
        || now_ms < issued
        || now_ms >= expires
        || expires <= issued
        || expires - issued > DELIVERY_PREVIEW_MAX_TTL_MS
    {
        return Err(denied());
    }
    Ok(manifest)
}
