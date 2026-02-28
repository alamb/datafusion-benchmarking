import * as gcp from "@pulumi/gcp";
import * as pulumi from "@pulumi/pulumi";

const gcpConfig = new pulumi.Config("gcp");
const region = gcpConfig.require("region");

export const cluster = new gcp.container.Cluster("benchmark-cluster", {
  name: "benchmark-cluster",
  location: region,
  enableAutopilot: true,
  releaseChannel: { channel: "REGULAR" },
  ipAllocationPolicy: {},
  deletionProtection: false,
});

export const clusterName = cluster.name;
export const clusterEndpoint = cluster.endpoint;
export const clusterLocation = cluster.location;
