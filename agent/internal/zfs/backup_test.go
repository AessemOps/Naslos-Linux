package zfs

import (
	"bytes"
	"context"
	"io"
	"strings"
	"testing"
)

// captureStreams replaces the two streaming seams so tests can pin the command
// line *and* decide what the "host" returns, without a host: these commands decide
// what a backup contains and what a restore overwrites.
type streamCapture struct {
	send    []string
	receive []string
}

func captureStreams(t *testing.T, sendBody string, sendErr error) *streamCapture {
	t.Helper()

	prevOut, prevIn := streamHostOut, streamHostIn
	t.Cleanup(func() { streamHostOut, streamHostIn = prevOut, prevIn })

	capture := &streamCapture{}
	streamHostOut = func(_ context.Context, name string, args ...string) (io.ReadCloser, func() error, error) {
		capture.send = append([]string{name}, args...)
		if sendErr != nil {
			return nil, nil, sendErr
		}
		return io.NopCloser(strings.NewReader(sendBody)), func() error { return nil }, nil
	}
	streamHostIn = func(_ context.Context, name string, args ...string) (io.WriteCloser, func() error, error) {
		capture.receive = append([]string{name}, args...)
		return &bufferWriteCloser{Buffer: &bytes.Buffer{}}, func() error { return nil }, nil
	}
	return capture
}

// bufferWriteCloser is a WriteCloser whose contents a test can inspect.
type bufferWriteCloser struct {
	*bytes.Buffer
}

func (b *bufferWriteCloser) Close() error { return nil }

func TestSendCommandLine(t *testing.T) {
	cases := []struct {
		name string
		opts SendStreamOptions
		want string
	}{
		{
			name: "full raw send",
			opts: SendStreamOptions{Dataset: "tank/data", To: "buddy-1", Raw: true},
			want: "send -w tank/data@buddy-1",
		},
		{
			name: "incremental raw send",
			opts: SendStreamOptions{Dataset: "tank/data", To: "buddy-2", From: "buddy-1", Raw: true},
			want: "send -w -i tank/data@buddy-1 tank/data@buddy-2",
		},
		{
			name: "not raw when asked",
			opts: SendStreamOptions{Dataset: "tank/data", To: "buddy-1"},
			want: "send tank/data@buddy-1",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			args, err := SendCommandLine(tc.opts)
			if err != nil {
				t.Fatalf("SendCommandLine: %v", err)
			}
			if got := strings.Join(args, " "); got != tc.want {
				t.Errorf("command = %q, want %q", got, tc.want)
			}
		})
	}
}

// TestSendCommandLineRefusals covers the input that must never reach the command
// line: a snapshot name is the one piece of backup input an operator types.
func TestSendCommandLineRefusals(t *testing.T) {
	cases := []struct {
		name string
		opts SendStreamOptions
	}{
		{"missing target", SendStreamOptions{Dataset: "tank/data"}},
		{"pool only", SendStreamOptions{Dataset: "tank", To: "s1"}},
		{"absolute dataset", SendStreamOptions{Dataset: "/tank/data", To: "s1"}},
		{"snapshot already qualified", SendStreamOptions{Dataset: "tank/data", To: "tank/data@s1"}},
		{"flag-looking name", SendStreamOptions{Dataset: "tank/data", To: "-R"}},
		{"slash in name", SendStreamOptions{Dataset: "tank/data", To: "a/b"}},
		{"same base and target", SendStreamOptions{Dataset: "tank/data", To: "s1", From: "s1"}},
		{"flag-looking base", SendStreamOptions{Dataset: "tank/data", To: "s2", From: "-i"}},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if _, err := SendCommandLine(tc.opts); err == nil {
				t.Error("SendCommandLine accepted input that must be refused")
			}
		})
	}
}

func TestSendStreamUsesTheStreamingSeam(t *testing.T) {
	capture := captureStreams(t, "zfs-stream-bytes", nil)
	client := NewClient(context.Background())

	stream, wait, err := client.SendStream(SendStreamOptions{Dataset: "tank/data", To: "buddy-1", Raw: true})
	if err != nil {
		t.Fatalf("SendStream: %v", err)
	}
	defer stream.Close()

	body, err := io.ReadAll(stream)
	if err != nil {
		t.Fatalf("reading stream: %v", err)
	}
	if string(body) != "zfs-stream-bytes" {
		t.Errorf("stream = %q, want the seam's bytes", body)
	}
	if err := wait(); err != nil {
		t.Errorf("wait: %v", err)
	}
	if got, want := strings.Join(capture.send, " "), "/usr/local/sbin/zfs send -w tank/data@buddy-1"; got != want {
		t.Errorf("command = %q, want %q", got, want)
	}
}

func TestReceiveStreamCommandLine(t *testing.T) {
	capture := captureStreams(t, "", nil)
	client := NewClient(context.Background())

	stdin, wait, err := client.ReceiveStream("tank/restored", true)
	if err != nil {
		t.Fatalf("ReceiveStream: %v", err)
	}
	if _, err := stdin.Write([]byte("stream")); err != nil {
		t.Fatalf("writing to receive: %v", err)
	}
	if err := stdin.Close(); err != nil {
		t.Fatalf("closing receive: %v", err)
	}
	if err := wait(); err != nil {
		t.Errorf("wait: %v", err)
	}
	if got, want := strings.Join(capture.receive, " "), "/usr/local/sbin/zfs receive -F tank/restored"; got != want {
		t.Errorf("command = %q, want %q", got, want)
	}
}

func TestEstimateSendCommandLineAndParsing(t *testing.T) {
	calls := captureRun(t, "size\t1073741824\n")
	client := NewClient(context.Background())

	bytes, err := client.EstimateSend(SendStreamOptions{Dataset: "tank/data", To: "buddy-1", From: "buddy-0", Raw: true})
	if err != nil {
		t.Fatalf("EstimateSend: %v", err)
	}
	if bytes != 1073741824 {
		t.Errorf("bytes = %d, want 1073741824", bytes)
	}
	// -n (dry run) and -P (parsable) must precede the rest of the send arguments,
	// which are otherwise identical to a real send.
	got := (*calls)[0]
	want := "/usr/local/sbin/zfs send -n -P -w -i tank/data@buddy-0 tank/data@buddy-1"
	if got != want {
		t.Errorf("command = %q, want %q", got, want)
	}
}

func TestEstimateSendWithoutSize(t *testing.T) {
	captureRun(t, "total estimated size is 1G\n")
	client := NewClient(context.Background())

	if _, err := client.EstimateSend(SendStreamOptions{Dataset: "tank/data", To: "buddy-1"}); err == nil {
		t.Error("a dry run without a parsable size line was accepted")
	}
}

func TestSnapshotsWithGUID(t *testing.T) {
	calls := captureRun(t,
		"tank/data@buddy-0\t111\t1700000000\ntank/data@buddy-1\t222\t1700000100\n")
	client := NewClient(context.Background())

	snapshots, err := client.SnapshotsWithGUID("tank/data")
	if err != nil {
		t.Fatalf("SnapshotsWithGUID: %v", err)
	}
	if len(snapshots) != 2 {
		t.Fatalf("got %d snapshots, want 2", len(snapshots))
	}
	if snapshots[1].Name != "buddy-1" || snapshots[1].GUID != "222" || snapshots[1].Created != "1700000100" {
		t.Errorf("second snapshot = %+v, want buddy-1/222/1700000100", snapshots[1])
	}
	got := (*calls)[0]
	want := "/usr/local/sbin/zfs list -H -p -t snapshot -o name,guid,creation -s creation -r tank/data"
	if got != want {
		t.Errorf("command = %q, want %q", got, want)
	}
}

func TestBackupRefusalsNeedNoHost(t *testing.T) {
	// Seams are deliberately left alone here: validation must reject before
	// anything is executed.
	client := NewClient(context.Background())
	if _, err := client.SnapshotsWithGUID("nested"); err == nil {
		t.Error("a dataset without a pool was accepted")
	}
	if _, _, err := client.ReceiveStream("/absolute", false); err == nil {
		t.Error("an absolute dataset was accepted")
	}
	if _, _, err := client.SendStream(SendStreamOptions{Dataset: "tank/data"}); err == nil {
		t.Error("a send without a target snapshot was accepted")
	}
}
