
  # Mailpit - Email Testing
  mailpit:
    image: axllent/mailpit:latest
    container_name: {project_name}_mailpit
    restart: unless-stopped
    ports:
      - "${MAILPIT_SMTP_PORT:-1025}:1025"  # SMTP
      - "${MAILPIT_UI_PORT:-8025}:8025"    # Web UI
    environment:
      MP_MAX_MESSAGES: 5000
      MP_SMTP_AUTH_ACCEPT_ANY: 1
      MP_SMTP_AUTH_ALLOW_INSECURE: 1
