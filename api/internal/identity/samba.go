package identity

import (
	"fmt"
	"os/exec"
	"strings"
)

// SMBConfig holds Samba connection configuration.
type SMBConfig struct {
	// ContainerName is the name of the Samba container to exec into
	ContainerName string
	// Namespace is the Kubernetes namespace
	Namespace string
	// Kubeconfig is the path to kubeconfig (empty for in-cluster)
	Kubeconfig string
}

// SMBManager manages Samba passdb synchronization.
type SMBManager struct {
	config SMBConfig
}

// NewSMBManager creates a new Samba manager.
func NewSMBManager(config SMBConfig) *SMBManager {
	return &SMBManager{config: config}
}

// SetPassword sets the SMB password for a user using their NT hash.
func (m *SMBManager) SetPassword(uid, ntHash string) error {
	// Use pdbedit to set the NT hash directly
	// This avoids needing the plaintext password in Samba
	args := []string{
		"exec", m.config.ContainerName,
		"pdbedit", "--user", uid,
		"--set-nt-hash", ntHash,
	}

	cmd, err := m.buildCommand(args)
	if err != nil {
		return err
	}

	output, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("setting SMB password: %s: %w", string(output), err)
	}

	return nil
}

// CreateUser creates a new Samba user with a random password.
// The user must then set their password via SetPassword.
func (m *SMBManager) CreateUser(uid string) error {
	// Add user to Samba passdb with a random password
	// User will need to set password on first use
	args := []string{
		"exec", m.config.ContainerName,
		"pdbedit", "--create", "--user", uid,
	}

	cmd, err := m.buildCommand(args)
	if err != nil {
		return err
	}

	output, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("creating SMB user: %s: %w", string(output), err)
	}

	return nil
}

// RemoveUser removes a user from Samba passdb.
func (m *SMBManager) RemoveUser(uid string) error {
	args := []string{
		"exec", m.config.ContainerName,
		"pdbedit", "--delete", "--user", uid,
	}

	cmd, err := m.buildCommand(args)
	if err != nil {
		return err
	}

	output, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("removing SMB user: %s: %w", string(output), err)
	}

	return nil
}

// UserExists checks if a user exists in Samba passdb.
func (m *SMBManager) UserExists(uid string) bool {
	args := []string{
		"exec", m.config.ContainerName,
		"pdbedit", "--user", uid,
	}

	cmd, err := m.buildCommand(args)
	if err != nil {
		return false
	}

	output, err := cmd.CombinedOutput()
	if err != nil {
		return false
	}

	return strings.Contains(string(output), uid)
}

// buildCommand builds the kubectl exec command.
func (m *SMBManager) buildCommand(args []string) (*exec.Cmd, error) {
	cmdArgs := []string{}

	if m.config.Kubeconfig != "" {
		cmdArgs = append(cmdArgs, "--kubeconfig", m.config.Kubeconfig)
	}

	if m.config.Namespace != "" {
		cmdArgs = append(cmdArgs, "-n", m.config.Namespace)
	}

	cmdArgs = append(cmdArgs, args...)

	return exec.Command("kubectl", cmdArgs...), nil
}
