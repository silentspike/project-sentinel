use super::*;

/// Owned SQL facts for one customer read phase; no runtime authority is retained.
#[derive(Debug, Clone, Serialize)]
pub struct CustomerOverviewReadSnapshot {
    pub requests: Vec<CustomerRequestV1>,
    pub proposals: Vec<ProposalV1>,
    pub projects: Vec<CustomerOverviewProjectRead>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CustomerOverviewProjectRead {
    pub project: ProjectV1,
    pub agreement: AgreementV1,
}

impl WorkflowStore {
    pub fn customer_overview_read_snapshot(
        &self,
        tenant: &TenantId,
        customer_id: &str,
        request_id: Option<&str>,
    ) -> Result<CustomerOverviewReadSnapshot, WorkflowError> {
        tenant.validate()?;
        validate_identifier(customer_id)?;
        if let Some(id) = request_id {
            validate_identifier(id)?;
        }
        let key = serde_json::to_vec(&(tenant, customer_id, request_id))?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        // Retain owned facts only under the existing exact global input proof.
        // Entity getters keep their independent validation arenas on every miss.
        self.validated_read_snapshot(&mut connection, "company-customer-overview", &key, |tx| {
            let requests = match request_id {
                Some(id) => get_entity::<CustomerRequestV1>(tx, tenant, "request", id)?
                    .into_iter()
                    .collect(),
                None => customer_requests_in_snapshot(tx, tenant, customer_id)?,
            };
            if requests
                .iter()
                .any(|request| request.customer_id != customer_id)
            {
                return Err(WorkflowError::new(
                    WorkflowErrorCode::AuthorityConflict,
                    false,
                    "customer request is foreign",
                ));
            }
            let mut proposals = Vec::new();
            for request in &requests {
                for id in &request.proposal_ids {
                    validate_identifier(id)?;
                    let proposal: ProposalV1 =
                        get_entity(tx, tenant, "proposal", id)?.ok_or_else(corrupt)?;
                    validate_proposal(&proposal)?;
                    if proposal.request_id != request.request_id {
                        return Err(corrupt());
                    }
                    proposals.push(proposal);
                }
            }
            // Validate every owned project before the caller applies its request filter.
            let projects = customer_projects_in_snapshot(tx, tenant, customer_id)?;
            Ok(CustomerOverviewReadSnapshot {
                requests,
                proposals,
                projects,
            })
        })
    }

    /// A fresh batch fence, not a cached equality decision.
    pub fn company_projects_match_snapshot(
        &self,
        expected: &[ProjectV1],
    ) -> Result<bool, WorkflowError> {
        if expected.is_empty() {
            return Ok(true);
        }
        if expected.len() > MAX_AGGREGATE_ITEMS {
            return Err(invalid("customer project limit exceeded"));
        }
        // Cache SQL facts independently of the caller's expected project contents.
        let identities = expected
            .iter()
            .map(|p| (&p.tenant_id, &p.project_id))
            .collect::<Vec<_>>();
        let key = serde_json::to_vec(&identities)?;
        let mut connection = self.connection.lock().map_err(|_| persistence())?;
        let projects =
            self.validated_read_snapshot(&mut connection, "company-project-batch", &key, |tx| {
                expected
                    .iter()
                    .map(|project| {
                        validated_company_project_in_snapshot(
                            tx,
                            &project.tenant_id,
                            &project.project_id,
                        )
                    })
                    .collect::<Result<Vec<_>, WorkflowError>>()
            })?;
        Ok(projects
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.as_ref() == Some(expected)))
    }
}

pub(super) fn customer_requests_in_snapshot(
    connection: &Connection,
    tenant: &TenantId,
    customer_id: &str,
) -> Result<Vec<CustomerRequestV1>, WorkflowError> {
    let mut statement = connection.prepare(
        "SELECT entity_id FROM company_entities WHERE tenant_id=?1 AND entity_kind='request' AND json_extract(payload,'$.customer_id')=?2 ORDER BY entity_id LIMIT 129",
    )?;
    let ids = statement
        .query_map(params![tenant.0, customer_id], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() > MAX_AGGREGATE_ITEMS {
        return Err(invalid(
            "customer inbox limit exceeded; use an exact request ID",
        ));
    }
    ids.into_iter()
        .map(|id| {
            let request: CustomerRequestV1 =
                get_entity(connection, tenant, "request", &id)?.ok_or_else(corrupt)?;
            validate_customer_request(&request)?;
            if request.customer_id != customer_id {
                return Err(corrupt());
            }
            Ok(request)
        })
        .collect()
}

pub(super) fn customer_projects_in_snapshot(
    connection: &Connection,
    tenant: &TenantId,
    customer_id: &str,
) -> Result<Vec<CustomerOverviewProjectRead>, WorkflowError> {
    let mut statement = connection.prepare(
        "SELECT p.entity_id FROM company_entities p JOIN company_entities a ON a.tenant_id=p.tenant_id AND a.entity_kind='agreement' AND a.entity_id=json_extract(p.payload,'$.agreement_id') WHERE p.tenant_id=?1 AND p.entity_kind='project' AND json_extract(a.payload,'$.customer_id')=?2 ORDER BY p.entity_id LIMIT 129",
    )?;
    let ids = statement
        .query_map(params![tenant.0, customer_id], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if ids.len() > MAX_AGGREGATE_ITEMS {
        return Err(invalid("customer project limit exceeded"));
    }
    ids.into_iter()
        .map(|id| {
            // Each canonical entity read keeps its original independent proof budget.
            let project: ProjectV1 =
                get_entity(connection, tenant, "project", &id)?.ok_or_else(corrupt)?;
            validate_project(&project)?;
            let agreement: AgreementV1 =
                get_entity(connection, tenant, "agreement", &project.agreement_id)?
                    .ok_or_else(corrupt)?;
            validate_agreement(&agreement)?;
            if agreement.customer_id != customer_id
                || agreement.proposal_digest != project.agreement_digest
            {
                return Err(corrupt());
            }
            Ok(CustomerOverviewProjectRead { project, agreement })
        })
        .collect()
}

#[cfg(test)]
mod tests;
