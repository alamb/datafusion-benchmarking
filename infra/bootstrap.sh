#!/usr/bin/env bash
# One-time bootstrap for GitHub Actions → GCP authentication.
#
# Creates the WIF pool, OIDC provider, and gha-deployer service account
# with gcloud. After this, push to main and GHA takes over via Pulumi.
#
# Prerequisites: gcloud CLI authenticated with owner/editor on the project.
set -euo pipefail

PROJECT_ID="datafusion-benchmarking"
REPO="adriangb/datafusion-benchmarking"
POOL_ID="github-actions-pool"
PROVIDER_ID="github-actions"
SA_NAME="gha-deployer"
SA_EMAIL="${SA_NAME}@${PROJECT_ID}.iam.gserviceaccount.com"

echo "==> Setting gcloud project to ${PROJECT_ID}..."
gcloud config set project "$PROJECT_ID"

PROJECT_NUMBER=$(gcloud projects describe "$PROJECT_ID" --format="value(projectNumber)")

echo "==> Enabling required GCP APIs..."
gcloud services enable \
  container.googleapis.com \
  artifactregistry.googleapis.com \
  iam.googleapis.com \
  compute.googleapis.com \
  serviceusage.googleapis.com \
  iamcredentials.googleapis.com \
  cloudresourcemanager.googleapis.com \
  sts.googleapis.com

echo "==> Creating Workload Identity pool..."
gcloud iam workload-identity-pools create "$POOL_ID" \
  --location="global" \
  --display-name="GitHub Actions Pool" \
  2>/dev/null || echo "    (pool already exists)"

echo "==> Creating OIDC provider..."
gcloud iam workload-identity-pools providers create-oidc "$PROVIDER_ID" \
  --location="global" \
  --workload-identity-pool="$POOL_ID" \
  --display-name="GitHub Actions" \
  --issuer-uri="https://token.actions.githubusercontent.com" \
  --attribute-mapping="google.subject=assertion.sub,attribute.repository=assertion.repository" \
  --attribute-condition="assertion.repository=='${REPO}'" \
  2>/dev/null || echo "    (provider already exists)"

echo "==> Creating gha-deployer service account..."
gcloud iam service-accounts create "$SA_NAME" \
  --display-name="GitHub Actions Deployer" \
  2>/dev/null || echo "    (SA already exists)"

echo "==> Granting project-level roles to gha-deployer..."
ROLES=(
  roles/container.admin
  roles/iam.serviceAccountAdmin
  roles/iam.serviceAccountUser
  roles/iam.workloadIdentityPoolAdmin
  roles/serviceusage.serviceUsageAdmin
  roles/resourcemanager.projectIamAdmin
  roles/storage.admin
)
for ROLE in "${ROLES[@]}"; do
  gcloud projects add-iam-policy-binding "$PROJECT_ID" \
    --member="serviceAccount:${SA_EMAIL}" \
    --role="$ROLE" \
    --condition=None \
    --quiet
done

echo "==> Binding WIF pool to gha-deployer SA..."
POOL_NAME="projects/${PROJECT_NUMBER}/locations/global/workloadIdentityPools/${POOL_ID}"
gcloud iam service-accounts add-iam-policy-binding "$SA_EMAIL" \
  --role="roles/iam.workloadIdentityUser" \
  --member="principalSet://iam.googleapis.com/${POOL_NAME}/attribute.repository/${REPO}" \
  --quiet

WIF_PROVIDER="${POOL_NAME}/providers/${PROVIDER_ID}"

echo ""
echo "========================================="
echo "Bootstrap complete!"
echo ""
echo "Set these GitHub repository variables on ${REPO}:"
echo ""
echo "  GCP_PROJECT_ID                  = ${PROJECT_ID}"
echo "  GCP_REGION                      = us-central1"
echo "  GCP_WORKLOAD_IDENTITY_PROVIDER  = ${WIF_PROVIDER}"
echo "  GCP_SERVICE_ACCOUNT_EMAIL       = ${SA_EMAIL}"
echo "  PULUMI_ORG                      = <your-pulumi-org>"
echo "  PULUMI_USER                     = <your-pulumi-username>"
echo ""
echo "Also configure Pulumi OIDC trust:"
echo "  1. Go to https://app.pulumi.com/<org>/settings/oidc"
echo "  2. Add issuer: https://token.actions.githubusercontent.com"
echo "  3. Audience:   urn:pulumi:org:<org>"
echo "  4. Sub policy: repo:${REPO}:*"
echo ""
echo "Then: gcloud auth application-default login && cd infra && npm ci && pulumi up --stack dev"
echo "========================================="
