package zfs

import "fmt"

// ValidationError marks an error caused by caller input (a bad pool or dataset
// name, an unknown disk, an unsupported option) rather than by the node. The
// HTTP layer answers 400 for these and 500 for everything else, so a caller can
// tell "fix your request" from "the node failed" (NAS-003).
type ValidationError struct {
	Message string
}

func (e *ValidationError) Error() string { return e.Message }

// invalidf builds a ValidationError with printf formatting.
func invalidf(format string, args ...interface{}) error {
	return &ValidationError{Message: fmt.Sprintf(format, args...)}
}
