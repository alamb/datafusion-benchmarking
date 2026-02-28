import * as pulumi from "@pulumi/pulumi";
import * as k8s from "@pulumi/kubernetes";

const config = new pulumi.Config("services");
const infra = new pulumi.StackReference(config.require("infraStack"));

export const clusterName = infra.getOutput("cluster") as pulumi.Output<string>;
export const registryUrl = infra.getOutput("registry") as pulumi.Output<string>;
export const controllerSaEmail = infra.getOutput("controllerServiceAccount") as pulumi.Output<string>;

const clusterEndpoint = infra.getOutput("endpoint") as pulumi.Output<string>;
const clusterCaCert = infra.getOutput("caCert") as pulumi.Output<string>;
const clusterLocation = infra.getOutput("clusterRegion") as pulumi.Output<string>;

export const k8sProvider = new k8s.Provider("gke", {
  kubeconfig: pulumi.all([clusterName, clusterEndpoint, clusterCaCert]).apply(
    ([name, endpoint, caCert]) => JSON.stringify({
      apiVersion: "v1",
      kind: "Config",
      clusters: [{
        cluster: {
          "certificate-authority-data": caCert,
          server: `https://${endpoint}`,
        },
        name,
      }],
      contexts: [{
        context: { cluster: name, user: name },
        name,
      }],
      "current-context": name,
      users: [{
        name,
        user: {
          exec: {
            apiVersion: "client.authentication.k8s.io/v1beta1",
            command: "gke-gcloud-auth-plugin",
            installHint: "Install gke-gcloud-auth-plugin for kubectl",
            provideClusterInfo: true,
          },
        },
      }],
    })
  ),
});
