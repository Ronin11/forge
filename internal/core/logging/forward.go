package logging

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"log/slog"
)

// maxForwardLine bounds one captured stderr line. Longer lines are forwarded in
// chunks of this size, each flagged truncated=true, and reading continues: a
// panic trace must never stall the child on a full pipe or be lost.
const maxForwardLine = 64 << 10

// Forward re-logs a child Forge process's stderr through logger. Lines that are
// JSON log records (the child inherited FORGE_LOG_FORMAT=json) are re-emitted at
// their own level with their attrs; anything else — a panic, a runtime warning —
// is logged at warn with the line as the message. The caller supplies a logger
// whose component names the child (e.g. For("worker")); the child's own
// component attr is kept as child_component so the two never collide. Returns
// when r is closed, with the read error if there was one.
func Forward(ctx context.Context, r io.Reader, logger *slog.Logger) error {
	br := bufio.NewReaderSize(r, maxForwardLine)
	for {
		line, isPrefix, err := br.ReadLine()
		if len(line) > 0 {
			forwardLine(ctx, logger, line, isPrefix)
		}
		if err != nil {
			if err == io.EOF {
				return nil
			}
			return err
		}
	}
}

func forwardLine(ctx context.Context, logger *slog.Logger, line []byte, truncated bool) {
	if rec, ok := parseRecord(line); ok && !truncated {
		logger.LogAttrs(ctx, rec.level, rec.msg, rec.attrs...)
		return
	}
	attrs := []slog.Attr{slog.String("source", "child-stderr")}
	if truncated {
		attrs = append(attrs, slog.Bool("truncated", true))
	}
	logger.LogAttrs(ctx, slog.LevelWarn, string(line), attrs...)
}

type forwarded struct {
	level slog.Level
	msg   string
	attrs []slog.Attr
}

// parseRecord recognises slog's JSON shape: {"time":…,"level":…,"msg":…,…}.
func parseRecord(line []byte) (forwarded, bool) {
	if len(line) == 0 || line[0] != '{' {
		return forwarded{}, false
	}
	var raw map[string]any
	if err := json.Unmarshal(line, &raw); err != nil {
		return forwarded{}, false
	}
	levelText, hasLevel := raw["level"].(string)
	msg, hasMsg := raw["msg"].(string)
	if !hasLevel || !hasMsg {
		return forwarded{}, false
	}
	level, err := ParseLevel(levelText)
	if err != nil {
		return forwarded{}, false
	}
	out := forwarded{level: level, msg: msg}
	for k, v := range raw {
		switch k {
		case "time", "level", "msg":
			continue
		case "component":
			k = "child_component"
		}
		out.attrs = append(out.attrs, slog.Any(k, v))
	}
	return out, true
}
