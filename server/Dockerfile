FROM node:22-alpine
RUN mkdir -p /app /var/lib/hearfolio && chown -R node:node /app /var/lib/hearfolio
WORKDIR /app
COPY --chown=node:node server/relay.mjs ./relay.mjs
USER node
ENV HOST=0.0.0.0 PORT=8787 RELAY_STATE=/var/lib/hearfolio/devices.json
EXPOSE 8787
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s CMD node -e "fetch('http://127.0.0.1:8787/health').then(r=>{if(!r.ok)process.exit(1)}).catch(()=>process.exit(1))"
CMD ["node", "relay.mjs"]
