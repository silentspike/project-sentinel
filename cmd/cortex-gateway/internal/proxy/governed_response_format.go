package proxy

// All five classified company schemas carry their own response contract in the
// task prompt. Defer to that contract without selecting a model decision.
const governedResponseFormat = "Return only the JSON response specified by the governed company execution contract in the request. Use its exact schema, fields and permitted alternatives instead of the ambient action envelope. Choose the response independently. This format instruction grants no authority and supplies no decision or execution evidence."
