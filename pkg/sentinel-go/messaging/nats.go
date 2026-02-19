// Package messaging provides NATS JetStream connection management and
// stream definitions for all Go services in Project Sentinel.
// This is the SSOT for NATS infrastructure — all services import from here.
package messaging

import (
	"fmt"
	"log/slog"
	"time"

	"github.com/nats-io/nats.go"
	"github.com/nats-io/nats.go/jetstream"
)

// DefaultURL is the default NATS server address (localhost only).
const DefaultURL = "nats://127.0.0.1:4222"

// ConnectOpts configures the NATS connection.
type ConnectOpts struct {
	URL    string
	Name   string // client name for monitoring
	Logger *slog.Logger
}

// Connect establishes a NATS connection with reconnect handling and slog integration.
func Connect(opts ConnectOpts) (*nats.Conn, error) {
	url := opts.URL
	if url == "" {
		url = DefaultURL
	}
	logger := opts.Logger
	if logger == nil {
		logger = slog.Default()
	}

	nc, err := nats.Connect(url,
		nats.Name(opts.Name),
		nats.RetryOnFailedConnect(true),
		nats.MaxReconnects(-1), // infinite reconnects
		nats.ReconnectWait(2*time.Second),
		nats.DisconnectErrHandler(func(_ *nats.Conn, err error) {
			logger.Warn("nats disconnected", "error", err)
		}),
		nats.ReconnectHandler(func(nc *nats.Conn) {
			logger.Info("nats reconnected", "url", nc.ConnectedUrl())
		}),
		nats.ErrorHandler(func(_ *nats.Conn, _ *nats.Subscription, err error) {
			logger.Error("nats async error", "error", err)
		}),
	)
	if err != nil {
		return nil, fmt.Errorf("nats connect: %w", err)
	}

	return nc, nil
}

// JetStream returns a JetStream context from an existing NATS connection.
func JetStream(nc *nats.Conn) (jetstream.JetStream, error) {
	js, err := jetstream.New(nc)
	if err != nil {
		return nil, fmt.Errorf("jetstream init: %w", err)
	}
	return js, nil
}
