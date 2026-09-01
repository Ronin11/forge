package controlplane

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"

	"forge/internal/core/config"
	"forge/internal/core/store"
)

// BootstrapOptions are the inputs bootstrap cannot derive itself.
type BootstrapOptions struct {
	Home     string
	UserHome string // the operator's home, for the default projects root
	// WriteWorkerConfig writes the default worker.toml if absent; it lives in the
	// worker package so cmd/forge passes it in (the daemon does not import worker).
	WriteWorkerConfig func(path string) (written bool, err error)
	// ModeSeeds are the embedded default mode prompts, name → body (M4 fills it).
	ModeSeeds map[string]string
	Logger    *slog.Logger
}

// BootstrapReport says what bootstrap created, for the log and for doctor.
type BootstrapReport struct {
	Created []string
}

// Bootstrap runs on every daemon start, idempotently, without prompts or
// network (DESIGN.md §1.3): directories, token, default configs, mode seeds,
// the default project.
func Bootstrap(ctx context.Context, st *store.Store, o BootstrapOptions) (*BootstrapReport, error) {
	if o.Logger == nil {
		o.Logger = slog.New(slog.DiscardHandler)
	}
	rep := &BootstrapReport{}
	dirs := []string{"", "logs", "logs/plugins", "worker", "kb", "modes", "plugins", "deps", "backups"}
	for _, d := range dirs {
		p := filepath.Join(o.Home, d)
		if _, err := os.Stat(p); os.IsNotExist(err) {
			if err := os.MkdirAll(p, 0o700); err != nil {
				return nil, fmt.Errorf("create %s: %w", p, err)
			}
			rep.Created = append(rep.Created, p)
		} else if err != nil {
			return nil, fmt.Errorf("stat %s: %w", p, err)
		}
	}
	if err := os.Chmod(o.Home, 0o700); err != nil {
		return nil, fmt.Errorf("chmod %s: %w", o.Home, err)
	}
	tokenPath := filepath.Join(o.Home, TokenFile)
	if _, err := os.Stat(tokenPath); os.IsNotExist(err) {
		var b [32]byte
		if _, err := rand.Read(b[:]); err != nil {
			return nil, fmt.Errorf("generate token: %w", err)
		}
		if err := os.WriteFile(tokenPath, []byte(hex.EncodeToString(b[:])+"\n"), 0o600); err != nil {
			return nil, fmt.Errorf("write token: %w", err)
		}
		rep.Created = append(rep.Created, tokenPath)
	} else if err != nil {
		return nil, fmt.Errorf("stat token: %w", err)
	}
	if written, err := config.WriteDefaultConfig(filepath.Join(o.Home, "config.toml"), o.Home, o.UserHome); err != nil {
		return nil, err
	} else if written {
		rep.Created = append(rep.Created, filepath.Join(o.Home, "config.toml"))
	}
	if o.WriteWorkerConfig != nil {
		p := filepath.Join(o.Home, "worker.toml")
		if written, err := o.WriteWorkerConfig(p); err != nil {
			return nil, err
		} else if written {
			rep.Created = append(rep.Created, p)
		}
	}
	for name, body := range o.ModeSeeds {
		p := filepath.Join(o.Home, "modes", name+".md")
		if _, err := os.Stat(p); os.IsNotExist(err) {
			if err := os.WriteFile(p, []byte(body), 0o600); err != nil {
				return nil, fmt.Errorf("seed mode %s: %w", name, err)
			}
			rep.Created = append(rep.Created, p)
		}
	}
	if err := st.Write(ctx, func(tx *store.Tx) error { return tx.EnsureProject(ctx, "default") }); err != nil {
		return nil, fmt.Errorf("ensure default project: %w", err)
	}
	for _, p := range rep.Created {
		o.Logger.InfoContext(ctx, "bootstrap created", "path", p)
	}
	return rep, nil
}

// ReadToken reads <home>/token for TCP clients.
func ReadToken(home string) (string, error) {
	b, err := os.ReadFile(filepath.Join(home, TokenFile))
	if err != nil {
		return "", fmt.Errorf("read token: %w", err)
	}
	return string(trimNewline(b)), nil
}

func trimNewline(b []byte) []byte {
	for len(b) > 0 && (b[len(b)-1] == '\n' || b[len(b)-1] == '\r') {
		b = b[:len(b)-1]
	}
	return b
}
