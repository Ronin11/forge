package worker

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"strings"
	"sync"
	"time"
)

// netProxyDialTimeout bounds the upstream dial for both CONNECT tunnels and
// plain HTTP forwards.
const netProxyDialTimeout = 10 * time.Second

// NetProxy is the per-attempt egress proxy of DESIGN.md §19: an HTTP proxy
// (CONNECT tunneling for TLS, plain forwarding for http://) on the worker's
// loopback, with an exact-match host allowlist from the claim policy. A denied
// host answers 403 and fires the onDenied callback once per request, which the
// attempt turns into a `net.denied` event on its timeline.
//
// It listens on 127.0.0.1:<random port>, not a unix socket: the sandbox shares
// the network namespace (see Sandbox), and HTTP_PROXY consumers want host:port.
type NetProxy struct {
	ln       net.Listener
	srv      *http.Server
	allow    []string
	onDenied func(host string)
	log      *slog.Logger
	// transport is shared by plain-HTTP forwards; it never uses a proxy
	// itself and keeps idle connections until Close.
	transport *http.Transport
	done      chan struct{}
}

// StartNetProxy listens and serves until Close. allow entries are exact hosts
// ("api.anthropic.com") or host:port ("dev.home:11434") — no wildcards; an
// empty list denies everything. onDenied may be nil.
func StartNetProxy(allow []string, onDenied func(host string), log *slog.Logger) (*NetProxy, error) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return nil, fmt.Errorf("netproxy: listen: %w", err)
	}
	p := &NetProxy{
		ln: ln, allow: allow, onDenied: onDenied, log: log,
		transport: &http.Transport{Proxy: nil, DialContext: (&net.Dialer{Timeout: netProxyDialTimeout}).DialContext},
		done:      make(chan struct{}),
	}
	p.srv = &http.Server{Handler: p, ReadHeaderTimeout: netProxyDialTimeout}
	go func() {
		defer close(p.done)
		// Serve returns ErrServerClosed on Close; anything else means the
		// proxy died under the attempt and is worth a log line.
		if err := p.srv.Serve(ln); err != nil && err != http.ErrServerClosed {
			log.Warn("netproxy stopped", "error", err)
		}
	}()
	return p, nil
}

// URL is what HTTP_PROXY/HTTPS_PROXY are set to.
func (p *NetProxy) URL() string { return "http://" + p.ln.Addr().String() }

// Close stops the listener, interrupts open tunnels, and waits for the serve
// goroutine (STYLE §3: every goroutine has an owner that waits for it).
func (p *NetProxy) Close() error {
	err := p.srv.Close()
	<-p.done
	p.transport.CloseIdleConnections()
	if err != nil {
		return fmt.Errorf("netproxy: close: %w", err)
	}
	return nil
}

func (p *NetProxy) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method == http.MethodConnect {
		p.connect(w, r)
		return
	}
	p.forward(w, r)
}

// allowed matches host[:port] against the allowlist: an entry without a port
// matches the host on any port; with one, both must match. Hosts compare
// case-insensitively; there are no wildcards by design.
func (p *NetProxy) allowed(hostport, defaultPort string) bool {
	host, port := splitHostPort(hostport, defaultPort)
	for _, entry := range p.allow {
		eh, ep := splitHostPort(entry, "")
		if !strings.EqualFold(eh, host) {
			continue
		}
		if ep == "" || ep == port {
			return true
		}
	}
	return false
}

func splitHostPort(hostport, defaultPort string) (host, port string) {
	host, port, err := net.SplitHostPort(hostport)
	if err != nil {
		return hostport, defaultPort
	}
	return host, port
}

// deny answers 403 and reports the host. The body names the proxy so an agent
// reading its tool output can tell policy from outage.
func (p *NetProxy) deny(w http.ResponseWriter, host string) {
	p.log.Info("netproxy denied", "host", host)
	if p.onDenied != nil {
		p.onDenied(host)
	}
	http.Error(w, fmt.Sprintf("forge netproxy: host %s is not in the allowlist", host), http.StatusForbidden)
}

// connect is the CONNECT tunnel: allowlist check, dial, 200, then bytes both
// ways until either side closes.
func (p *NetProxy) connect(w http.ResponseWriter, r *http.Request) {
	if !p.allowed(r.Host, "443") {
		p.deny(w, r.Host)
		return
	}
	target := r.Host
	if _, _, err := net.SplitHostPort(target); err != nil {
		target = net.JoinHostPort(target, "443")
	}
	upstream, err := net.DialTimeout("tcp", target, netProxyDialTimeout)
	if err != nil {
		http.Error(w, fmt.Sprintf("forge netproxy: dial %s: %v", target, err), http.StatusBadGateway)
		return
	}
	hj, ok := w.(http.Hijacker)
	if !ok {
		if cerr := upstream.Close(); cerr != nil {
			p.log.Debug("netproxy close upstream", "error", cerr)
		}
		http.Error(w, "forge netproxy: cannot hijack connection", http.StatusInternalServerError)
		return
	}
	client, buf, err := hj.Hijack()
	if err != nil {
		if cerr := upstream.Close(); cerr != nil {
			p.log.Debug("netproxy close upstream", "error", cerr)
		}
		p.log.Warn("netproxy hijack", "error", err)
		return
	}
	_, werr := buf.WriteString("HTTP/1.1 200 Connection Established\r\n\r\n")
	if werr == nil {
		werr = buf.Flush()
	}
	if werr != nil {
		p.closePair(client, upstream)
		return
	}
	var wg sync.WaitGroup
	wg.Add(2)
	pipe := func(dst io.WriteCloser, src io.Reader) {
		defer wg.Done()
		if _, err := io.Copy(dst, src); err != nil {
			p.log.Debug("netproxy tunnel copy ended", "host", r.Host, "error", err)
		}
		// Closing the write side unblocks the peer copy; errors here are the
		// normal end of a torn-down tunnel.
		if err := dst.Close(); err != nil {
			p.log.Debug("netproxy tunnel close", "error", err)
		}
	}
	go pipe(upstream, buf)
	pipe(client, upstream)
	wg.Wait()
}

func (p *NetProxy) closePair(a, b net.Conn) {
	for _, c := range []net.Conn{a, b} {
		if err := c.Close(); err != nil {
			p.log.Debug("netproxy close", "error", err)
		}
	}
}

// forward relays one absolute-form plain-HTTP request.
func (p *NetProxy) forward(w http.ResponseWriter, r *http.Request) {
	host := r.Host
	if host == "" {
		host = r.URL.Host
	}
	if host == "" {
		http.Error(w, "forge netproxy: request has no host", http.StatusBadRequest)
		return
	}
	if !p.allowed(host, "80") {
		p.deny(w, host)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), time.Minute)
	defer cancel()
	out := r.Clone(ctx)
	out.RequestURI = ""
	if out.URL.Scheme == "" {
		out.URL.Scheme = "http"
	}
	if out.URL.Host == "" {
		out.URL.Host = host
	}
	// Hop-by-hop headers belong to the client↔proxy leg, not upstream.
	for _, h := range []string{"Proxy-Connection", "Proxy-Authorization", "Connection", "Keep-Alive", "Te", "Trailer", "Transfer-Encoding", "Upgrade"} {
		out.Header.Del(h)
	}
	resp, err := p.transport.RoundTrip(out)
	if err != nil {
		http.Error(w, fmt.Sprintf("forge netproxy: %v", err), http.StatusBadGateway)
		return
	}
	defer func() {
		if cerr := resp.Body.Close(); cerr != nil {
			p.log.Debug("netproxy close response", "error", cerr)
		}
	}()
	header := w.Header()
	for k, vs := range resp.Header {
		for _, v := range vs {
			header.Add(k, v)
		}
	}
	w.WriteHeader(resp.StatusCode)
	if _, err := io.Copy(w, resp.Body); err != nil {
		p.log.Debug("netproxy forward copy ended", "host", host, "error", err)
	}
}
