package server

import (
	"context"
	"testing"

	corev1 "k8s.io/api/core/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime"
	"k8s.io/client-go/kubernetes"
	"k8s.io/client-go/kubernetes/fake"
)

func svc(name, release string, ports ...corev1.ServicePort) *corev1.Service {
	return &corev1.Service{
		ObjectMeta: metav1.ObjectMeta{
			Name:      name,
			Namespace: "naslos-apps",
			Annotations: map[string]string{
				"meta.helm.sh/release-name": release,
			},
		},
		Spec: corev1.ServiceSpec{Ports: ports},
	}
}

func discovererFor(objects ...runtime.Object) appServiceDiscoverer {
	return appServiceDiscoverer{client: func() (kubernetes.Interface, error) {
		return fake.NewSimpleClientset(objects...), nil
	}}
}

func TestServicesForReleaseFiltersAndOrders(t *testing.T) {
	d := discovererFor(
		svc("zeta", "hello", corev1.ServicePort{Name: "http", Port: 80}),
		svc("alpha", "hello", corev1.ServicePort{Name: "http", Port: 8080}, corev1.ServicePort{Name: "https", Port: 443}),
		svc("other", "other-release", corev1.ServicePort{Name: "http", Port: 80}),
	)
	got, err := d.ServicesForRelease(context.Background(), "naslos-apps", "hello")
	if err != nil {
		t.Fatalf("discover: %v", err)
	}
	if len(got) != 3 {
		t.Fatalf("got %d services, want 3: %+v", len(got), got)
	}
	if got[0].Name != "alpha" || got[0].Port != 443 || got[0].Scheme != "https" {
		t.Fatalf("first service = %+v, want alpha:443 https (sorted, https scheme)", got[0])
	}
	if got[2].Name != "zeta" || got[2].Scheme != "http" {
		t.Fatalf("last service = %+v, want zeta:80 http", got[2])
	}
}

func TestPortScheme(t *testing.T) {
	cases := []struct {
		port corev1.ServicePort
		want string
	}{
		{corev1.ServicePort{Name: "http", Port: 80}, "http"},
		{corev1.ServicePort{Name: "https", Port: 8443}, "https"},
		{corev1.ServicePort{Name: "web-tls", Port: 9443}, "https"},
		{corev1.ServicePort{Name: "", Port: 443}, "https"},
		{corev1.ServicePort{Name: "metrics", Port: 9090}, "http"},
	}
	for _, tc := range cases {
		if got := portScheme(tc.port); got != tc.want {
			t.Errorf("portScheme(%+v) = %q, want %q", tc.port, got, tc.want)
		}
	}
}
