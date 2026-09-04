// smtp.go is the outbound boundary for mode "smtp": one connection per message,
// TLS the way the server wants it, and PLAIN auth when a username is set. The
// stdlib's net/smtp is enough — a notification is a single small message, and a
// pooled connection would only add a failure mode.
package main

import (
	"context"
	"crypto/tls"
	"fmt"
	"net"
	"net/smtp"
	"strings"
	"time"
)

// smtpTimeout bounds one send, dial through QUIT.
const smtpTimeout = 30 * time.Second

// smtpMailer sends mail through one server.
type smtpMailer struct {
	cfg  smtpConfig
	from string
	to   string
	now  func() time.Time
}

func newSMTPMailer(cfg smtpConfig, from, to string, now func() time.Time) *smtpMailer {
	return &smtpMailer{cfg: cfg, from: from, to: to, now: now}
}

// send delivers one message. Every error says which step failed, since an SMTP
// misconfiguration is nearly always in one specific step (TLS, auth, or the
// envelope).
func (m *smtpMailer) send(ctx context.Context, msg outgoing) (err error) {
	messageID, err := newMessageID(hostOf(m.from))
	if err != nil {
		return err
	}
	body, err := buildMIME(m.from, m.to, msg, messageID, m.now())
	if err != nil {
		return err
	}
	addr := net.JoinHostPort(m.cfg.Host, fmt.Sprint(m.cfg.Port))
	cctx, cancel := context.WithTimeout(ctx, smtpTimeout)
	defer cancel()
	conn, err := dialSMTP(cctx, m.cfg, addr)
	if err != nil {
		return err
	}
	client, err := smtp.NewClient(conn, m.cfg.Host)
	if err != nil {
		if cerr := conn.Close(); cerr != nil {
			return fmt.Errorf("smtp handshake with %s: %w (and closing the connection: %v)", addr, err, cerr)
		}
		return fmt.Errorf("smtp handshake with %s: %w", addr, err)
	}
	defer func() {
		if cerr := client.Quit(); cerr != nil && err == nil {
			err = fmt.Errorf("smtp quit: %w", cerr)
		}
	}()
	if m.cfg.TLS == tlsSTARTTLS {
		if err := client.StartTLS(&tls.Config{ServerName: m.cfg.Host, MinVersion: tls.VersionTLS12}); err != nil {
			return fmt.Errorf("starttls with %s: %w", addr, err)
		}
	}
	if m.cfg.Username != "" {
		auth := smtp.PlainAuth("", m.cfg.Username, m.cfg.Password, m.cfg.Host)
		if err := client.Auth(auth); err != nil {
			return fmt.Errorf("smtp auth as %s: %w", m.cfg.Username, err)
		}
	}
	if err := client.Mail(addressOf(m.from)); err != nil {
		return fmt.Errorf("smtp MAIL FROM %s: %w", m.from, err)
	}
	if err := client.Rcpt(addressOf(m.to)); err != nil {
		return fmt.Errorf("smtp RCPT TO %s: %w", m.to, err)
	}
	w, err := client.Data()
	if err != nil {
		return fmt.Errorf("smtp DATA: %w", err)
	}
	if _, err := w.Write(body); err != nil {
		return fmt.Errorf("write message body: %w", err)
	}
	if err := w.Close(); err != nil {
		return fmt.Errorf("finish message: %w", err)
	}
	return nil
}

// dialSMTP opens the connection the configured TLS mode calls for.
func dialSMTP(ctx context.Context, cfg smtpConfig, addr string) (net.Conn, error) {
	d := &net.Dialer{}
	if cfg.TLS == tlsImplicit {
		conn, err := (&tls.Dialer{NetDialer: d, Config: &tls.Config{ServerName: cfg.Host, MinVersion: tls.VersionTLS12}}).DialContext(ctx, "tcp", addr)
		if err != nil {
			return nil, fmt.Errorf("dial %s over TLS: %w", addr, err)
		}
		return conn, nil
	}
	conn, err := d.DialContext(ctx, "tcp", addr)
	if err != nil {
		return nil, fmt.Errorf("dial %s: %w", addr, err)
	}
	return conn, nil
}

// hostOf is the domain half of an address, used for the Message-ID.
func hostOf(address string) string {
	addr := addressOf(address)
	if i := strings.LastIndex(addr, "@"); i >= 0 && i < len(addr)-1 {
		return addr[i+1:]
	}
	return ""
}
