package server

import (
	"context"
	"strings"

	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"

	"github.com/AessemOps/Naslos-Linux/api/internal/helm"
)

// managedByHelm matches the label Helm stamps on every resource it manages.
const managedByHelm = "app.kubernetes.io/managed-by=Helm"

// discoverPlatformReleases finds releases in the platform namespace by their
// Helm labels instead of running a Helm list. The Helm SDK stores releases as
// Secrets, so a Helm list would need `list secrets` in the platform namespace -
// which would also expose the proxy secret, LDAP bind password and Authelia
// keys. Reading workload labels instead needs only read access to workloads.
func (s *Server) discoverPlatformReleases(ctx context.Context) ([]helm.App, error) {
	client, err := s.kubernetesClient()
	if err != nil {
		return nil, err
	}

	seen := make(map[string]helm.App)
	add := func(name string, labels map[string]string) {
		if name == "" || name == s.platformRelease {
			return
		}
		if _, ok := seen[name]; ok {
			return
		}
		app := helm.App{Name: name, Namespace: s.namespace, Status: "backfilled"}
		if chart := labels["helm.sh/chart"]; chart != "" {
			app.Chart, app.Version = splitChartLabel(chart)
		}
		if version := labels["app.kubernetes.io/version"]; version != "" {
			app.Version = version
		}
		seen[name] = app
	}

	var firstErr error
	listDeployments, err := client.AppsV1().Deployments(s.namespace).List(ctx, metav1.ListOptions{LabelSelector: managedByHelm})
	if err != nil {
		firstErr = err
	} else {
		for i := range listDeployments.Items {
			item := &listDeployments.Items[i]
			add(item.Annotations["meta.helm.sh/release-name"], item.Labels)
		}
	}
	listStatefulSets, err := client.AppsV1().StatefulSets(s.namespace).List(ctx, metav1.ListOptions{LabelSelector: managedByHelm})
	if err != nil && firstErr == nil {
		firstErr = err
	} else if err == nil {
		for i := range listStatefulSets.Items {
			item := &listStatefulSets.Items[i]
			add(item.Annotations["meta.helm.sh/release-name"], item.Labels)
		}
	}
	listDaemonSets, err := client.AppsV1().DaemonSets(s.namespace).List(ctx, metav1.ListOptions{LabelSelector: managedByHelm})
	if err != nil && firstErr == nil {
		firstErr = err
	} else if err == nil {
		for i := range listDaemonSets.Items {
			item := &listDaemonSets.Items[i]
			add(item.Annotations["meta.helm.sh/release-name"], item.Labels)
		}
	}
	// A chart may render only a Service (no workload), so include Helm-labeled
	// Services. ConfigMaps are deliberately excluded: they can carry secrets
	// (Authelia's jwt_secret lives in one).
	listServices, err := client.CoreV1().Services(s.namespace).List(ctx, metav1.ListOptions{LabelSelector: managedByHelm})
	if err != nil && firstErr == nil {
		firstErr = err
	} else if err == nil {
		for i := range listServices.Items {
			item := &listServices.Items[i]
			add(item.Annotations["meta.helm.sh/release-name"], item.Labels)
		}
	}

	out := make([]helm.App, 0, len(seen))
	for _, app := range seen {
		out = append(out, app)
	}
	if len(out) == 0 && firstErr != nil {
		return nil, firstErr
	}
	return out, nil
}

// splitChartLabel turns "naslos-0.1.0" into ("naslos", "0.1.0"), keeping the
// whole string as the name when there is no parseable version suffix.
func splitChartLabel(label string) (string, string) {
	idx := strings.LastIndex(label, "-")
	if idx <= 0 || idx == len(label)-1 {
		return label, ""
	}
	candidate := label[idx+1:]
	if !strings.ContainsAny(candidate, "0123456789") {
		return label, ""
	}
	return label[:idx], candidate
}
