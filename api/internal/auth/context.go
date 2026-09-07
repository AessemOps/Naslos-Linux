package auth

import (
	"context"
	"strings"
)

// contextKey is a private type for context keys.
type contextKey int

const userContextKey contextKey = iota

// UserInfo holds authenticated user information.
type UserInfo struct {
	Username    string
	Groups      []string
	Email       string
	DisplayName string
}

// HasGroup checks if the user belongs to a group.
func (u *UserInfo) HasGroup(group string) bool {
	for _, g := range u.Groups {
		if strings.EqualFold(g, group) {
			return true
		}
	}
	return false
}

// IsAdmin checks if the user is an admin.
func (u *UserInfo) IsAdmin() bool {
	return u.HasGroup("nasos_admins")
}

// WithUser adds user info to context.
func WithUser(ctx context.Context, user *UserInfo) context.Context {
	return context.WithValue(ctx, userContextKey, user)
}

// UserFromContext retrieves user info from context.
func UserFromContext(ctx context.Context) *UserInfo {
	user, ok := ctx.Value(userContextKey).(*UserInfo)
	if !ok {
		return nil
	}
	return user
}
