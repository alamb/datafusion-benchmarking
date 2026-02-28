# Set your project
export PROJECT_ID="datafusion-benchmarking"
export PROJECT_NUMBER=$(gcloud projects describe $PROJECT_ID --format='value(projectNumber)')

# Enable required APIs
gcloud services enable container.googleapis.com \
artifactregistry.googleapis.com \
iam.googleapis.com \
compute.googleapis.com \
--project $PROJECT_ID

# Create the Workload Identity Pool
gcloud iam workload-identity-pools create github-actions-pool \
--project=$PROJECT_ID \
--location=global \
--display-name="GitHub Actions Pool"

# Create the OIDC Provider (trusts GitHub's token issuer)
gcloud iam workload-identity-pools providers create-oidc github-actions \
--project=$PROJECT_ID \
--location=global \
--workload-identity-pool=github-actions-pool \
--display-name="GitHub Actions" \
--issuer-uri="https://token.actions.githubusercontent.com" \
--attribute-mapping="google.subject=assertion.sub,attribute.repository=assertion.repository" \
--attribute-condition="assertion.repository=='adriangb/datafusion-benchmarking'"

# Create the service account GHA will impersonate
gcloud iam service-accounts create gha-deployer \
--project=$PROJECT_ID \
--display-name="GitHub Actions Deployer"

# Grant it the roles it needs
for ROLE in roles/container.admin roles/artifactregistry.admin roles/iam.serviceAccountAdmin roles/iam.workloadIdentityPoolAdmin;
do
gcloud projects add-iam-policy-binding $PROJECT_ID \
    --member="serviceAccount:gha-deployer@${PROJECT_ID}.iam.gserviceaccount.com" \
    --role="$ROLE"
done

# Allow the WIF pool to impersonate this service account
gcloud iam service-accounts add-iam-policy-binding \
gha-deployer@${PROJECT_ID}.iam.gserviceaccount.com \
--project=$PROJECT_ID \
--role="roles/iam.workloadIdentityUser" \
--member="principalSet://iam.googleapis.com/projects/${PROJECT_NUMBER}/locations/global/workloadIdentityPools/github-actions-pool/attribute.repository/adriangb/datafusion-benchmarking"
