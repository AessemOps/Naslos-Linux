package chartsrepo

import (
	"context"
	"fmt"

	"golang.org/x/crypto/ssh"
	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
)

// Credentials are the resolved secrets for one source.
type Credentials struct {
	// Token is an HTTPS access token (AuthToken).
	Token string
	// SSHKey is a PEM-encoded private key (AuthSSH).
	SSHKey []byte
}

// CredentialsProvider resolves a source's credentials.
type CredentialsProvider interface {
	Resolve(ctx context.Context, src *Source) (Credentials, error)
}

// insecureHostKeyCallback accepts any SSH host key. SSH deploy keys are only
// used against the admin-configured remote and known_hosts pinning is a
// documented follow-up; this keeps DNS-based development remotes working.
var insecureHostKeyCallback ssh.HostKeyCallback = ssh.InsecureIgnoreHostKey()

// K8sCredentialsProvider reads credentials from Kubernetes Secrets.
type K8sCredentialsProvider struct {
	Client kubernetes.Interface
	// DefaultNamespace is used when a source does not set CredentialsNamespace.
	DefaultNamespace string
}

// Resolve loads the token or SSH key named by the source from its Secret.
func (p *K8sCredentialsProvider) Resolve(ctx context.Context, src *Source) (Credentials, error) {
	if p == nil || p.Client == nil {
		return Credentials{}, fmt.Errorf("source %q: no Kubernetes client for credentials", src.Name)
	}
	namespace := src.CredentialsNamespace
	if namespace == "" {
		namespace = p.DefaultNamespace
	}
	if namespace == "" {
		return Credentials{}, fmt.Errorf("source %q: no credentials namespace configured", src.Name)
	}
	secret, err := p.Client.CoreV1().Secrets(namespace).Get(ctx, src.CredentialsSecret, metav1.GetOptions{})
	if err != nil {
		return Credentials{}, fmt.Errorf("source %q: reading Secret %s/%s: %w", src.Name, namespace, src.CredentialsSecret, err)
	}
	return credentialsFromSecret(src, secret)
}

func credentialsFromSecret(src *Source, secret *corev1.Secret) (Credentials, error) {
	switch src.Auth {
	case AuthToken:
		raw, ok := secret.Data[src.TokenKeyOr()]
		if !ok {
			return Credentials{}, fmt.Errorf("source %q: Secret %q has no key %q", src.Name, src.CredentialsSecret, src.TokenKeyOr())
		}
		return Credentials{Token: string(raw)}, nil
	case AuthSSH:
		raw, ok := secret.Data[src.SSHKeyKeyOr()]
		if !ok {
			return Credentials{}, fmt.Errorf("source %q: Secret %q has no key %q", src.Name, src.CredentialsSecret, src.SSHKeyKeyOr())
		}
		return Credentials{SSHKey: raw}, nil
	default:
		return Credentials{}, nil
	}
}

// StaticCredentialsProvider is a test/single-source helper.
type StaticCredentialsProvider struct {
	Creds Credentials
	Err   error
}

// Resolve returns the fixed credentials.
func (p StaticCredentialsProvider) Resolve(context.Context, *Source) (Credentials, error) {
	return p.Creds, p.Err
}
