package intercept

import "time"

// Mode controls how inbound interception behaves.
type Mode string

const (
	ModeAuto   Mode = "auto"
	ModeManual Mode = "manual"
)

// RequestAction describes the decision taken before Provider.Send().
type RequestAction string

const (
	RequestForward RequestAction = "forward"
	RequestModify  RequestAction = "modify"
	RequestDrop    RequestAction = "drop"
)

// RequestDecision is the result of inbound interception.
type RequestDecision struct {
	Action        RequestAction
	Reason        string
	ContextSuffix string
}

// PendingRequest captures a holdable inbound request.
type PendingRequest struct {
	ID        string
	RoomID    string
	AgentName string
	Reason    string
	CreatedAt time.Time
}

func Forward(reason string) RequestDecision {
	return RequestDecision{Action: RequestForward, Reason: reason}
}

func Modify(reason, contextSuffix string) RequestDecision {
	return RequestDecision{
		Action:        RequestModify,
		Reason:        reason,
		ContextSuffix: contextSuffix,
	}
}

func Drop(reason string) RequestDecision {
	return RequestDecision{Action: RequestDrop, Reason: reason}
}
