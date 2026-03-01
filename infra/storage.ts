import * as gcp from "@pulumi/gcp";
import * as pulumi from "@pulumi/pulumi";

const project = gcp.config.project!;
const gcpConfig = new pulumi.Config("gcp");
const region = gcpConfig.require("region");

// GCS bucket for caching deterministic benchmark data (TPC-H, ClickBench, TPC-DS).
// 30-day lifecycle auto-deletes stale cached data.
export const dataCacheBucket = new gcp.storage.Bucket("benchdata", {
  name: `${project}-benchdata`,
  location: region,
  uniformBucketLevelAccess: true,
  lifecycleRules: [{
    action: { type: "Delete" },
    condition: { age: 30 },
  }],
});
