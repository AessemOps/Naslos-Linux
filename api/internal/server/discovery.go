package server

import (
	"context"
	"sort"
	"strings"

	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"

	"github.com/AessemOps/Naslos-Linux/api/internal/apps"
)

// appServiceDiscoverer finds the Services a release rendered, so an app whose
// manifest declares no route target can still be exposed.
type appServiceDiscoverer struct {
	client func() (kubernetes.Interface, error)
}

// ServicesForRelease lists the release's Services in namespace. A Service is
// matched by the Helm release-name annotation, falling back to the standard
// app.kubernetes.io/instance label.
func (d appServiceDiscoverer) ServicesForRelease(ctx context.Context, namespace, release string) ([]apps.DiscoveredService, error) {
	client, err := d.client()
	if err != nil {
		return nil, err
	}
	list, err := client.CoreV1().Services(namespace).List(ctx, metav1.ListOptions{})
	if err != nil {
		return nil, err
	}

	out := make([]apps.DiscoveredService, 0)
	for i := range list.Items {
		svc := &list.Items[i]
		if svc.Annotations["meta.helm.sh/release-name"] != release &&
			svc.Labels["app.kubernetes.io/instance"] != release {
			continue
		}
		for _, port := range svc.Spec.Ports {
			out = append(out, apps.DiscoveredService{
				Name:     svc.Name,
				Port:     int(port.Port),
				Scheme:   portScheme(port),
				PortName: port.Name,
			})
		}
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Name != out[j].Name {
			return out[i].Name < out[j].Name
		}
		return out[i].Port < out[j].Port
	})
	return out, nil
}

// portScheme guesses the Traefik service scheme from a Service port: a TLS-ish
// name or port 443 upgrades to https.
func portScheme(port corev1.ServicePort) string {
	name := strings.ToLower(port.Name)
	if strings.Contains(name, "https") || strings.Contains(name, "tls") || port.Port == 443 {
		return "https"
	}
	return "http"
}
