import * as k8s from "@pulumi/kubernetes";
import * as pulumi from "@pulumi/pulumi";
import { k8sProvider, registryUrl, controllerSaEmail, runnerSaEmail, sccacheGcsBucket } from "./provider";

const config = new pulumi.Config();

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
      "iam.gke.io/gcp-service-account": controllerSaEmail,
    },
  },
}, { provider: k8sProvider, dependsOn: [ns] });

// K8s service account for benchmark runner pods with Workload Identity annotation
const runnerKsa = new k8s.core.v1.ServiceAccount("benchmark-runner", {
  metadata: {
    name: "benchmark-runner",
    namespace: "benchmarking",
    annotations: {
      "iam.gke.io/gcp-service-account": runnerSaEmail,
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

// Logfire token secret (optional, set via `pulumi config set --secret logfireToken`)
const logfireToken = config.getSecret("logfireToken");

const logfireSecret = logfireToken ? new k8s.core.v1.Secret("logfire-token", {
  metadata: {
    name: "logfire-token",
    namespace: "benchmarking",
  },
  stringData: {
    token: logfireToken,
  },
}, { provider: k8sProvider, dependsOn: [ns] }) : undefined;

// RBAC — allow the controller SA to manage batch Jobs in its namespace
const controllerRole = new k8s.rbac.v1.Role("benchmark-controller", {
  metadata: {
    name: "benchmark-controller",
    namespace: "benchmarking",
  },
  rules: [{
    apiGroups: ["batch"],
    resources: ["jobs"],
    verbs: ["create", "get", "list", "delete"],
  }],
}, { provider: k8sProvider, dependsOn: [ns] });

const controllerRoleBinding = new k8s.rbac.v1.RoleBinding("benchmark-controller", {
  metadata: {
    name: "benchmark-controller",
    namespace: "benchmarking",
  },
  roleRef: {
    apiGroup: "rbac.authorization.k8s.io",
    kind: "Role",
    name: "benchmark-controller",
  },
  subjects: [{
    kind: "ServiceAccount",
    name: "benchmark-controller",
    namespace: "benchmarking",
  }],
}, { provider: k8sProvider, dependsOn: [controllerRole, controllerKsa] });

// StorageClass for benchmark runner workspace volumes
const hyperdiskBalanced = new k8s.storage.v1.StorageClass("hyperdisk-balanced", {
  metadata: { name: "hyperdisk-balanced" },
  provisioner: "pd.csi.storage.gke.io",
  parameters: { type: "hyperdisk-balanced" },
  reclaimPolicy: "Delete",
  volumeBindingMode: "WaitForFirstConsumer",
  allowVolumeExpansion: true,
}, { provider: k8sProvider });

// Controller StatefulSet

const imageTag = config.get("imageTag") || "latest";
const controllerImage = pulumi.interpolate`${registryUrl}/controller:${imageTag}`;
const runnerImage = pulumi.interpolate`${registryUrl}/runner:${imageTag}`;

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
          ports: [{ name: "health", containerPort: 8080, protocol: "TCP" }],
          startupProbe: {
            httpGet: { path: "/healthz", port: "health" },
            failureThreshold: 30,
            periodSeconds: 2,
          },
          livenessProbe: {
            httpGet: { path: "/healthz", port: "health" },
            periodSeconds: 30,
            failureThreshold: 3,
          },
          readinessProbe: {
            httpGet: { path: "/readyz", port: "health" },
            periodSeconds: 10,
            failureThreshold: 3,
          },
          env: [
            { name: "DATABASE_URL", value: "sqlite:///data/benchmark.db" },
            { name: "BENCHMARK_CONFIG", value: JSON.stringify({
              allowed_users: [
                "alamb", "Dandandan", "adriangb", "rluvaton", "geoffreyclaude",
                "xudong963", "zhuqi-lucas", "Omega359", "comphead", "klion26",
                "gabotechs", "Jefffrey", "etseidl",
              ],
              repos: {
                "adriangb/datafusion": {
                  standard: [
                    "tpch", "tpch10", "tpch_mem", "tpch_mem10",
                    "clickbench_partitioned", "clickbench_extended",
                    "clickbench_1", "clickbench_pushdown",
                    "external_aggr", "tpcds",
                  ],
                  criterion: [
                    "sql_planner", "in_list", "case_when",
                    "aggregate_vectorized", "aggregate_query_sql",
                    "with_hashes", "range_and_generate_series",
                    "sort", "left", "strpos", "substr_index",
                    "character_length", "reset_plan_states",
                    "replace", "plan_reuse",
                  ],
                },
              },
            }) },
            { name: "POLL_INTERVAL_SECS", value: "30" },
            { name: "RECONCILE_INTERVAL_SECS", value: "10" },
            { name: "K8S_NAMESPACE", value: "benchmarking" },
            { name: "RUNNER_IMAGE", value: runnerImage },
            { name: "RUST_LOG", value: "info" },
            ...(logfireToken ? [
              { name: "LOGFIRE_TOKEN", valueFrom: { secretKeyRef: { name: "logfire-token", key: "token" } } },
            ] : []),
            { name: "SCCACHE_GCS_BUCKET", value: sccacheGcsBucket },
            { name: "LOGFIRE_SERVICE_NAME", value: "benchmark-controller" },
            { name: "LOGFIRE_ENVIRONMENT", value: "production" },
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
}, { provider: k8sProvider, dependsOn: [ns, controllerKsa, githubSecret, ...(logfireSecret ? [logfireSecret] : [])] });
