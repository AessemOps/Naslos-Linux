package server

import (
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/AessemOps/Naslos-Linux/agent/internal/zfs"
)

// TestWriteClientErrorStatus pins the status mapping: caller-fixable input is a
// 400, a node-side failure is a 500, and a wrapped validation error still
// classifies (the zfs client wraps some of them with context).
func TestWriteClientErrorStatus(t *testing.T) {
	cases := []struct {
		name string
		err  error
		want int
	}{
		{"validation", &zfs.ValidationError{Message: "invalid pool name"}, http.StatusBadRequest},
		{"wrapped validation", fmt.Errorf("wiping /dev/sdb: %w", &zfs.ValidationError{Message: "no"}), http.StatusBadRequest},
		{"backend failure", errors.New("zpool exited 1"), http.StatusInternalServerError},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			rec := httptest.NewRecorder()
			writeClientError(rec, tc.err)
			if rec.Code != tc.want {
				t.Errorf("status = %d, want %d", rec.Code, tc.want)
			}
		})
	}
}
