#!/usr/bin/env bash
# Bootstrap script — run once manually before Pulumi can self-manage.
#
# These resources form the chicken-and-egg: GHA needs them to authenticate
# to GCP, so Pulumi (which runs in GHA) can't create them.
#
# Everything that CAN be managed by Pulumi (Artifact Registry permissions,
# controller SA, GKE cluster, etc.) lives in the Pulumi code instead.
set -euo pipefail

PROJECT_ID="datafusion-benchmarking"
REPO="adriangb/datafusion-benchmarking"
PROJECT_NUMBER=$(gcloud projects describe "$PROJECT_ID" --format='value(projectNumber)')

# 1. Enable required APIs
gcloud services enable \
  container.googleapis.com \
  artifactregistry.googleapis.com \
  iam.googleapis.com \
  compute.googleapis.com \
  --project "$PROJECT_ID"

# 2. Create the Workload Identity Pool
gcloud iam workload-identity-pools create github-actions-pool \
  --project="$PROJECT_ID" \
  --location=global \
  --display-name="GitHub Actions Pool"

# 3. Create the OIDC Provider (trusts GitHub's token issuer)
gcloud iam workload-identity-pools providers create-oidc github-actions \
  --project="$PROJECT_ID" \
  --location=global \
  --workload-identity-pool=github-actions-pool \
  --display-name="GitHub Actions" \
  --issuer-uri="https://token.actions.githubusercontent.com" \
  --attribute-mapping="google.subject=assertion.sub,attribute.repository=assertion.repository" \
  --attribute-condition="assertion.repository=='${REPO}'"

# 4. Create the service account GHA will impersonate
gcloud iam service-accounts create gha-deployer \
  --project="$PROJECT_ID" \
  --display-name="GitHub Actions Deployer"

# 5. Grant minimum project-level roles needed to run Pulumi + kubectl.
#    Scoped permissions (e.g. Artifact Registry) are managed by Pulumi.
for ROLE in \
  roles/container.admin \
  roles/iam.serviceAccountAdmin \
  roles/iam.serviceAccountUser \
  roles/iam.workloadIdentityPoolAdmin; do
  gcloud projects add-iam-policy-binding "$PROJECT_ID" \
    --member="serviceAccount:gha-deployer@${PROJECT_ID}.iam.gserviceaccount.com" \
    --role="$ROLE"
done

# 6. Allow the WIF pool to impersonate the gha-deployer SA
gcloud iam service-accounts add-iam-policy-binding \
  "gha-deployer@${PROJECT_ID}.iam.gserviceaccount.com" \
  --project="$PROJECT_ID" \
  --role="roles/iam.workloadIdentityUser" \
  --member="principalSet://iam.googleapis.com/projects/${PROJECT_NUMBER}/locations/global/workloadIdentityPools/github-actions-pool/attribute.repository/${REPO}"

echo ""
echo "Bootstrap complete. Now set these as GitHub repo variables on ${REPO}:"
echo "  GCP_PROJECT_ID                  = ${PROJECT_ID}"
echo "  GCP_WORKLOAD_IDENTITY_PROVIDER  = projects/${PROJECT_NUMBER}/locations/global/workloadIdentityPools/github-actions-pool/providers/github-actions"
echo "  GCP_SERVICE_ACCOUNT_EMAIL       = gha-deployer@${PROJECT_ID}.iam.gserviceaccount.com"
