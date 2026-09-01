package worker

import (
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"sync"
	"testing"

	"forge/internal/core/logging"
)

// deniedRecorder collects onDenied callbacks; the proxy calls them from its
// serve goroutines.
type deniedRecorder struct {
	mu    sync.Mutex
	hosts []string
}

func (d *deniedRecorder) record(host string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.hosts = append(d.hosts, host)
}

func (d *deniedRecorder) list() []string {
	d.mu.Lock()
	defer d.mu.Unlock()
	return append([]string(nil), d.hosts...)
}

func startProxy(t *testing.T, allow []string) (*NetProxy, *deniedRecorder) {
	t.Helper()
	rec := &deniedRecorder{}
	p, err := StartNetProxy(allow, rec.record, logging.Discard().For("test"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := p.Close(); err != nil {
			t.Errorf("close proxy: %v", err)
		}
	})
	return p, rec
}

func proxyClient(t *testing.T, p *NetProxy, base *http.Client) *http.Client {
	t.Helper()
	u, err := url.Parse(p.URL())
	if err != nil {
		t.Fatal(err)
	}
	tr := &http.Transport{Proxy: http.ProxyURL(u)}
	if base != nil {
		if bt, ok := base.Transport.(*http.Transport); ok {
			tr.TLSClientConfig = bt.TLSClientConfig
		}
	}
	return &http.Client{Transport: tr}
}

func TestNetProxyForwardsAllowedHTTP(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if _, err := io.WriteString(w, "hello "+r.URL.Path); err != nil {
			t.Error(err)
		}
	}))
	defer ts.Close()
	p, rec := startProxy(t, []string{"127.0.0.1"})
	resp, err := proxyClient(t, p, nil).Get(ts.URL + "/x")
	if err != nil {
		t.Fatal(err)
	}
	body, err := io.ReadAll(resp.Body)
	if cerr := resp.Body.Close(); err != nil || cerr != nil {
		t.Fatal(err, cerr)
	}
	if resp.StatusCode != http.StatusOK || string(body) != "hello /x" {
		t.Errorf("status %d body %q", resp.StatusCode, body)
	}
	if got := rec.list(); len(got) != 0 {
		t.Errorf("denied = %v", got)
	}
}

func TestNetProxyDeniesUnlistedHost(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		t.Error("upstream reached despite denial")
	}))
	defer ts.Close()
	p, rec := startProxy(t, []string{"api.anthropic.com"})
	resp, err := proxyClient(t, p, nil).Get(ts.URL)
	if err != nil {
		t.Fatal(err)
	}
	body, err := io.ReadAll(resp.Body)
	if cerr := resp.Body.Close(); err != nil || cerr != nil {
		t.Fatal(err, cerr)
	}
	if resp.StatusCode != http.StatusForbidden || !strings.Contains(string(body), "not in the allowlist") {
		t.Errorf("status %d body %q", resp.StatusCode, body)
	}
	if got := rec.list(); len(got) != 1 || !strings.HasPrefix(got[0], "127.0.0.1") {
		t.Errorf("denied = %v", got)
	}
}

func TestNetProxyDeniesOnPortMismatch(t *testing.T) {
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		t.Error("upstream reached despite denial")
	}))
	defer ts.Close()
	// The entry pins a port the test server does not listen on.
	p, rec := startProxy(t, []string{"127.0.0.1:1"})
	resp, err := proxyClient(t, p, nil).Get(ts.URL)
	if err != nil {
		t.Fatal(err)
	}
	if cerr := resp.Body.Close(); cerr != nil {
		t.Fatal(cerr)
	}
	if resp.StatusCode != http.StatusForbidden || len(rec.list()) != 1 {
		t.Errorf("status %d denied %v", resp.StatusCode, rec.list())
	}
}

func TestNetProxyTunnelsAllowedTLS(t *testing.T) {
	ts := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		if _, err := io.WriteString(w, "secure"); err != nil {
			t.Error(err)
		}
	}))
	defer ts.Close()
	p, rec := startProxy(t, []string{"127.0.0.1"})
	resp, err := proxyClient(t, p, ts.Client()).Get(ts.URL)
	if err != nil {
		t.Fatal(err)
	}
	body, err := io.ReadAll(resp.Body)
	if cerr := resp.Body.Close(); err != nil || cerr != nil {
		t.Fatal(err, cerr)
	}
	if resp.StatusCode != http.StatusOK || string(body) != "secure" {
		t.Errorf("status %d body %q", resp.StatusCode, body)
	}
	if got := rec.list(); len(got) != 0 {
		t.Errorf("denied = %v", got)
	}
}

func TestNetProxyRefusesCONNECTToUnlistedHost(t *testing.T) {
	ts := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		t.Error("upstream reached despite denial")
	}))
	defer ts.Close()
	p, rec := startProxy(t, nil) // empty allowlist denies everything
	_, err := proxyClient(t, p, ts.Client()).Get(ts.URL)
	if err == nil || !strings.Contains(err.Error(), "Forbidden") {
		t.Errorf("want a Forbidden CONNECT refusal, got %v", err)
	}
	if got := rec.list(); len(got) != 1 {
		t.Errorf("denied = %v", got)
	}
}
