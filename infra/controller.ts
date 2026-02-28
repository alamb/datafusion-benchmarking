import * as k8s from "@pulumi/kubernetes";
import * as pulumi from "@pulumi/pulumi";
import * as gcp from "@pulumi/gcp";
import { cluster, clusterEndpoint, clusterName, clusterLocation } from "./cluster";
import { controllerServiceAccountEmail } from "./identity";
import { registryUrl } from "./registry";

const config = new pulumi.Config();
const project = gcp.config.project!;
const gcpConfig = new pulumi.Config("gcp");
const region = gcpConfig.require("region");

// K8s provider configured against the GKE cluster
const k8sProvider = new k8s.Provider("gke", {
  kubeconfig: pulumi.interpolate`apiVersion: v1
clusters:
- cluster:
    certificate-authority-data: ${cluster.masterAuth.clusterCaCertificate}
    server: https://${clusterEndpoint}
  name: ${clusterName}
contexts:
- context:
    cluster: ${clusterName}
    user: ${clusterName}
  name: ${clusterName}
current-context: ${clusterName}
kind: Config
users:
- name: ${clusterName}
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1beta1
      command: gke-gcloud-auth-plugin
      installHint: Install gke-gcloud-auth-plugin for kubectl
      provideClusterInfo: true`,
});

// Namespace
const ns = new k8s.core.v1.Namespace("benchmarking", {
  metadata: { name: "benchmarking" },
}, { provider: k8sProvider });

// K8s service account with Workload Identity annotation
const controllerKsa = new k8s.core.v1.ServiceAccount("benchmark-controller", {
  metadata: {
    name: "benchmark-controller",
    namespace: "benchmarking",
    annotations: {
      "iam.gke.io/gcp-service-account": controllerServiceAccountEmail,
    },
  },
}, { provider: k8sProvider, dependsOn: [ns] });

// GitHub token secret (value set via `pulumi config set --secret githubToken`)
const githubToken = config.requireSecret("githubToken");

const githubSecret = new k8s.core.v1.Secret("github-token", {
  metadata: {
    name: "github-token",
    namespace: "benchmarking",
  },
  stringData: {
    token: githubToken,
  },
}, { provider: k8sProvider, dependsOn: [ns] });

// Controller StatefulSet
const controllerImage = pulumi.interpolate`${registryUrl}/controller:latest`;
const runnerImage = pulumi.interpolate`${registryUrl}/runner:latest`;

export const controllerStatefulSet = new k8s.apps.v1.StatefulSet("benchmark-controller", {
  metadata: {
    name: "benchmark-controller",
    namespace: "benchmarking",
  },
  spec: {
    replicas: 1,
    serviceName: "benchmark-controller",
    selector: { matchLabels: { app: "benchmark-controller" } },
    template: {
      metadata: { labels: { app: "benchmark-controller" } },
      spec: {
        serviceAccountName: "benchmark-controller",
        terminationGracePeriodSeconds: 30,
        containers: [{
          name: "controller",
          image: controllerImage,
          env: [
            { name: "DATABASE_URL", value: "sqlite:///data/benchmark.db" },
            { name: "WATCHED_REPOS", value: "apache/datafusion:apache/arrow-rs" },
            { name: "POLL_INTERVAL_SECS", value: "30" },
            { name: "RECONCILE_INTERVAL_SECS", value: "10" },
            { name: "K8S_NAMESPACE", value: "benchmarking" },
            { name: "RUNNER_IMAGE", value: runnerImage },
            { name: "RUST_LOG", value: "info" },
            {
              name: "GITHUB_TOKEN",
              valueFrom: { secretKeyRef: { name: "github-token", key: "token" } },
            },
          ],
          resources: {
            requests: { cpu: "250m", memory: "256Mi" },
            limits: { cpu: "500m", memory: "512Mi" },
          },
          volumeMounts: [{
            name: "controller-db",
            mountPath: "/data",
          }],
        }],
        // Run on default (non-Performance) compute class, non-spot
        nodeSelector: {
          "kubernetes.io/os": "linux",
        },
      },
    },
    volumeClaimTemplates: [{
      metadata: { name: "controller-db" },
      spec: {
        accessModes: ["ReadWriteOnce"],
        storageClassName: "premium-rwo",
        resources: {
          requests: { storage: "1Gi" },
        },
      },
    }],
  },
}, { provider: k8sProvider, dependsOn: [ns, controllerKsa, githubSecret] });
