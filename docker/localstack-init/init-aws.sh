#!/bin/bash
set -e

echo "Initializing LocalStack AWS resources..."

# Create DynamoDB table for documents
awslocal dynamodb create-table \
  --table-name Documents \
  --attribute-definitions \
    AttributeName=PK,AttributeType=S \
    AttributeName=SK,AttributeType=S \
  --key-schema \
    AttributeName=PK,KeyType=HASH \
    AttributeName=SK,KeyType=RANGE \
  --billing-mode PAY_PER_REQUEST

# Create DynamoDB table for MLS messages (offline queue)
awslocal dynamodb create-table \
  --table-name MLSMessages \
  --attribute-definitions \
    AttributeName=PK,AttributeType=S \
    AttributeName=SK,AttributeType=S \
  --key-schema \
    AttributeName=PK,KeyType=HASH \
    AttributeName=SK,KeyType=RANGE \
  --billing-mode PAY_PER_REQUEST

# Create SQS queue for async message processing
awslocal sqs create-queue \
  --queue-name collab-updates

echo "LocalStack initialization complete!"
