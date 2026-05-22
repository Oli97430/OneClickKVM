# Politique de sécurité

## Versions supportées

OneClick KVM est en alpha. Seule la version la plus récente reçoit des
correctifs de sécurité.

| Version | Supportée |
|---------|-----------|
| 0.1.x   | ✅        |
| < 0.1   | ❌        |

## Signaler une vulnérabilité

**Ne créez pas d'issue publique** pour les failles de sécurité — utilisez
l'un de ces canaux privés :

1. **GitHub Security Advisory** (préféré) :
   [Reporter une vulnérabilité en privé](https://github.com/Oli97430/OneClickKVM/security/advisories/new)
   — visible uniquement par les mainteneurs jusqu'à publication coordonnée.

2. **Email** : `tarraw974@gmail.com` avec sujet préfixé `[SECURITY]`.

### Que mettre dans le rapport ?

- **Description** : nature de la faille (auth bypass, RCE, info disclosure,
  DoS, etc.)
- **Impact** : ce qu'un attaquant peut obtenir (lecture/écriture, élévation
  de privilège, lateral movement sur le LAN, ...)
- **Reproduction** : étapes minimales, code de PoC si possible
- **Version affectée** + OS + topologie réseau de test
- **Suggestion de fix** (optionnel)

### Engagement

- Accusé de réception sous **72 heures** (best effort, projet maintenu sur
  temps libre)
- Évaluation initiale + estimation de gravité sous **7 jours**
- Patch et release coordonnée sous **30 jours** pour les Critiques/Hauts,
  négociable au cas par cas
- Crédit dans les release notes (sauf demande contraire)

## Modèle de menace

Le modèle de menace détaillé est dans
[`docs/SECURITY.md`](docs/SECURITY.md) : ce qui est protégé (chiffrement
de transit, authentification mutuelle, identité long-terme), ce qui ne
l'est **pas** (machine compromise = jeu fini), et les choix
cryptographiques motivés (AES-256-GCM, X25519, Ed25519, BLAKE3, HKDF-SHA256,
DPAPI).

## Pas dans le scope

- **Attaques physiques** : si quelqu'un a accès console à un PC appairé,
  il peut tout faire.
- **Compromission d'un PC pair** : l'app fait confiance à un pair déjà
  appairé. Voir le modèle de menace pour les limites.
- **Bugs UI / crash sans escalation** : utiliser le tracker public.
- **Attaques DoS LAN** (saturation broadcast) : best effort, pas une
  priorité tant que l'app reste cantonnée à un LAN de confiance.
- **SmartScreen / signature Authenticode** : assumé. Pas de cert Authenticode
  prévu (cf. README). La vérification d'intégrité passe par le SHA-256 publié
  sur chaque release — comparer avant exécution.
