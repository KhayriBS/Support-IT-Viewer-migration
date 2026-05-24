---
Titre : Conception et développement d’une plateforme intelligente de support informatique à distance
Type   : Rapport de Projet de Fin d’Études (PFE)
Durée  : Stage de six (6) mois
---

# INTRODUCTION GÉNÉRALE

L’informatique est devenue, au cours des deux dernières décennies, un instrument central du fonctionnement des entreprises. Les postes de travail, les serveurs internes, les outils métiers et les applications collaboratives forment aujourd’hui un environnement complexe dans lequel toute interruption de service se traduit immédiatement par une perte de productivité, des retards de livraison et, dans de nombreux cas, par un impact financier non négligeable. Dans ce contexte, le support informatique constitue une fonction stratégique, dont la qualité et la réactivité conditionnent directement la continuité de l’activité.

Historiquement, le support informatique reposait sur une intervention physique du technicien auprès de l’utilisateur. Cette approche, bien qu’efficace dans les petites structures, est rapidement devenue inadaptée à mesure que les parcs informatiques se sont étendus, que les sites se sont multipliés et que le télétravail s’est généralisé. L’apparition de la prise en main à distance, à travers des outils comme TeamViewer, AnyDesk, Chrome Remote Desktop ou plus récemment RustDesk, a profondément transformé la manière dont les équipes IT interviennent. Elle a permis de réduire les délais de résolution, de supprimer les déplacements coûteux et d’offrir un service unifié à l’ensemble des collaborateurs.

Cependant, les solutions actuelles présentent un certain nombre de limites lorsqu’elles sont confrontées aux exigences réelles des entreprises. Plusieurs d’entre elles sont commerciales et imposent des licences onéreuses dès que l’usage dépasse un cadre strictement personnel. D’autres reposent sur des infrastructures externes peu compatibles avec les politiques de sécurité internes. Enfin, la plupart d’entre elles n’intègrent pas, ou seulement de manière marginale, des fonctionnalités modernes comme l’assistance par intelligence artificielle, la collecte temps réel de métriques système ou une gestion fine des permissions par session.

C’est dans ce contexte que s’inscrit le présent projet de fin d’études. Il a pour objectif de concevoir et de développer une plateforme intelligente de support informatique à distance, fondée sur une architecture résolument client‑to‑client. La même application desktop, développée avec Tauri pour la couche d’interface et Rust pour la logique native, est installée sur l’ensemble des postes. Chaque utilisateur peut alors, selon la situation, jouer le rôle de demandeur d’assistance ou celui d’assistant, sans qu’il existe d’espace administrateur séparé ni de distinction figée entre les profils. Un serveur central, développé avec Spring Boot, assure l’authentification, la gestion des utilisateurs, le suivi des clients connectés, l’orchestration des sessions et la signalisation WebRTC.

La problématique qui guide ce travail peut être formulée ainsi : *comment concevoir une plateforme de support informatique à distance sécurisée, performante et enrichie par l’intelligence artificielle, qui permette à deux postes utilisateurs de communiquer directement en pair à pair, sans dépendre d’un espace administrateur central, tout en assurant la traçabilité et la maîtrise des autorisations ?*

La solution proposée repose sur cinq piliers techniques complémentaires. Premièrement, une application desktop unique, légère et multi‑rôles, distribuée à l’identique sur tous les postes. Deuxièmement, un backend central Spring Boot, qui assume également le rôle de serveur de signalisation WebRTC via WebSocket. Troisièmement, un canal temps réel pair à pair construit sur WebRTC, complété par des serveurs STUN et TURN pour la traversée des NAT. Quatrièmement, un pipeline de capture d’écran et d’encodage vidéo H.264 capable de soutenir un flux temps réel exploitable par l’assistant. Cinquièmement, une couche d’intelligence artificielle, capable d’analyser une capture d’écran et de proposer ou d’exécuter, sous contrôle, des actions intelligentes destinées à accélérer le diagnostic.

Le présent rapport est structuré en trois chapitres, encadrés par une introduction générale, une conclusion et une webgraphie. Le **premier chapitre** présente le contexte général du projet, l’étude de l’existant, la méthodologie de gestion de projet retenue et le langage de modélisation utilisé. Le **deuxième chapitre** est consacré à la planification du projet et à la spécification des besoins, fonctionnels comme non fonctionnels, ainsi qu’à la conception globale, à travers les diagrammes de cas d’utilisation et de classes. Le **troisième chapitre** se concentre sur le premier sprint de réalisation : analyse, préparation de l’environnement, conception détaillée des premiers cas d’utilisation, diagrammes de séquence et mise en œuvre des interfaces initiales. La conclusion générale dresse un bilan du travail accompli et trace les perspectives d’évolution de la plateforme.

---

# CHAPITRE 1 : CONTEXTE GÉNÉRAL DU PROJET

## 1.1 Cadre Général

Le présent projet de fin d’études prend place dans un contexte professionnel où la dématérialisation des outils de travail et la mobilité des collaborateurs imposent aux services informatiques une réactivité que les méthodes classiques d’intervention ne peuvent plus garantir. Les entreprises attendent désormais de leur support qu’il soit capable, en quelques minutes, d’identifier l’origine d’un dysfonctionnement, de l’analyser et de le résoudre, indépendamment de la localisation géographique de l’utilisateur concerné. La prise en main à distance constitue, dans cette perspective, un levier essentiel de productivité et de qualité de service.

Au‑delà de la simple rapidité, plusieurs enjeux structurants gouvernent la mise en place d’une solution de support à distance. Le premier est celui de la **sécurité** : la prise de contrôle d’un poste, même temporaire, expose des données sensibles et exige une gestion stricte de l’authentification, des autorisations et de la traçabilité. Le second est celui de l’**accessibilité** : la solution doit pouvoir s’adapter à des environnements réseau hétérogènes, derrière des pare‑feux ou des NAT, sans imposer de configurations particulières aux utilisateurs finaux. Le troisième est celui de la **réduction des déplacements**, qui se traduit par une baisse mesurable des coûts logistiques et une diminution des indisponibilités. Enfin, le quatrième enjeu est celui de la **continuité de service** : la solution doit demeurer disponible et réactive y compris dans des conditions réseau dégradées.

Le projet vise à apporter une réponse cohérente à l’ensemble de ces enjeux à travers une approche résolument **client‑to‑client**. Concrètement, deux utilisateurs équipés de la même application desktop peuvent se connecter directement l’un à l’autre, après authentification et accord explicite. L’un d’eux endosse le rôle de demandeur d’assistance, l’autre celui d’assistant. La connexion temps réel s’établit en pair à pair grâce à WebRTC, le serveur central n’intervenant que pour authentifier les utilisateurs, orchestrer la création de la session et relayer les messages de signalisation. Cette approche garantit une faible latence, une scalabilité naturelle et une séparation claire entre le plan de contrôle, hébergé sur le serveur, et le plan de données, transporté directement entre les deux clients.

## 1.2 Présentation de l’Organisme d’Accueil

*Section à compléter par les informations propres à l’organisme d’accueil.*

## 1.3 Organisme de la Société

*Section à compléter par l’organigramme et la description des services de la société d’accueil.*

## 1.4 Étude de l’Existant

L’étude de l’existant constitue une étape déterminante de tout projet logiciel. Elle permet d’identifier les solutions disponibles sur le marché, d’analyser leurs forces et leurs faiblesses, puis de positionner la solution proposée par rapport aux pratiques en vigueur. Dans le domaine du support informatique à distance, plusieurs produits dominent aujourd’hui le paysage technologique.

**TeamViewer** est probablement la solution la plus connue. Elle propose une couverture multi‑plateforme étendue, une grande maturité fonctionnelle et un modèle commercial attrayant pour les très grandes entreprises. Toutefois, elle repose sur une infrastructure propriétaire externe, applique une politique de licences contraignante dès que l’usage devient professionnel et n’expose qu’une part limitée de son fonctionnement interne, ce qui restreint les possibilités de personnalisation.

**AnyDesk** se distingue par la performance de son moteur de transmission vidéo et par la légèreté de son client. La solution est appréciée pour sa fluidité, même dans des conditions réseau modestes. Elle souffre néanmoins des mêmes limites que ses concurrents commerciaux : code source fermé, dépendance à des serveurs externes et flexibilité limitée pour intégrer des fonctionnalités métier spécifiques telles que l’assistance par IA.

**Chrome Remote Desktop**, proposé par Google, mise sur la simplicité d’usage : l’ensemble du fonctionnement passe par le navigateur, sans installation lourde. Cette approche a un coût en termes de fonctionnalités, puisque le contrôle clavier/souris est limité, le transfert de fichiers réduit et l’intégration avec des outils tiers quasi inexistante. La solution convient à un usage personnel mais ne couvre pas les exigences d’un support professionnel.

**RustDesk** est l’alternative open‑source la plus mature. Elle reprend l’architecture générale des outils précédents tout en exposant son code, ce qui en fait une excellente référence technique. Cependant, sa configuration reste complexe, son interface est moins polie que celle des solutions commerciales et son écosystème n’intègre pas nativement de fonctionnalités d’assistance par intelligence artificielle ni de gestion fine des sessions à des fins de traçabilité métier.

L’examen croisé de ces solutions permet de dégager un certain nombre d’axes d’amélioration. C’est précisément sur ces axes que se positionne le projet proposé, dont la valeur ajoutée se manifeste à travers les choix techniques suivants : (i) l’adoption de **WebRTC** comme protocole de transport, gage de standardisation et d’interopérabilité ; (ii) la prise en charge native d’un **contrôle distant complet**, incluant souris, clavier, défilement et entrées avancées ; (iii) un **transfert de fichiers** pair à pair sur DataChannel, sans relais externe ; (iv) la collecte et l’affichage en temps réel de **métriques système** (CPU, RAM, disque) ; (v) l’intégration d’une **assistance par intelligence artificielle**, capable d’analyser le contexte visuel et de suggérer ou d’exécuter des actions ; (vi) une **application desktop légère**, fondée sur Tauri et Rust, permettant des binaires de taille réduite et une consommation mémoire raisonnable ; et (vii) une **architecture client‑to‑client**, dans laquelle chaque utilisateur dispose de la même application et peut alternativement demander ou apporter une assistance.

Le tableau suivant synthétise la comparaison des solutions existantes avec l’approche proposée.

| Critère                          | TeamViewer | AnyDesk | Chrome Remote Desktop | RustDesk | Solution proposée |
|----------------------------------|------------|---------|-----------------------|----------|-------------------|
| Open source                      | Non        | Non     | Non                   | Oui      | Oui (projet académique) |
| Architecture client‑to‑client    | Partielle  | Partielle | Partielle           | Oui      | Oui, native |
| Assistance par IA intégrée       | Non        | Non     | Non                   | Non      | Oui |
| Personnalisation métier          | Faible     | Faible  | Très faible           | Moyenne  | Élevée |
| Gestion fine des permissions     | Moyenne    | Moyenne | Faible                | Moyenne  | Élevée |
| Hébergement maîtrisé             | Non        | Non     | Non                   | Oui      | Oui |
| Légèreté du client               | Moyenne    | Élevée  | Élevée                | Élevée   | Élevée (Tauri + Rust) |

## 1.5 Méthodologie de Travail

### 1.5.1 Comparaison des Méthodologies

*Section à compléter par la comparaison des méthodologies (cascade, V, agiles, Scrum, Kanban, XP).*

### 1.5.2 La Méthodologie Scrum

*Section à compléter par la présentation de Scrum, ses rôles, artefacts et principes fondateurs.*

### 1.5.3 Les Événements Clés du Cycle Scrum

*Section à compléter par la description des événements Scrum (Sprint, Sprint Planning, Daily, Review, Retrospective).* 

Dans le cadre du présent projet de fin d’études, le cycle Scrum a été adapté à un fonctionnement individuel encadré, où l’étudiant joue le rôle d’équipe de développement, l’encadrant professionnel celui de Product Owner et l’encadrant académique celui de garant méthodologique. Les sprints ont une durée de deux semaines. Chaque sprint commence par une session de planification au cours de laquelle les user stories à traiter sont sélectionnées dans le backlog ; il se poursuit par un travail quotidien rythmé par de courts points d’avancement avec les encadrants, et se conclut par une revue de sprint où le livrable est présenté, suivie d’une rétrospective qui formalise les enseignements à reporter sur le sprint suivant.

## 1.6 Méthodologie UML

Le langage de modélisation unifié, plus connu sous l’acronyme **UML** (*Unified Modeling Language*), s’est imposé comme un standard *de facto* pour la conception des systèmes logiciels. Il propose un ensemble cohérent de diagrammes complémentaires, qui permettent d’aborder le système selon différents points de vue : fonctionnel, structurel, comportemental ou architectural. Le choix d’UML pour ce projet répond à une exigence double : disposer d’un formalisme rigoureux pour communiquer avec les encadrants et garantir la traçabilité entre les besoins exprimés et les composants finalement implémentés.

Quatre diagrammes UML ont été retenus comme axes principaux de la conception. Le **diagramme de cas d’utilisation** sert à recenser les fonctionnalités du système du point de vue des acteurs ; il joue un rôle pivot dans l’expression des besoins. Le **diagramme de classes** modélise la structure statique du système : il décrit les entités manipulées, leurs attributs, leurs méthodes et leurs relations, et constitue le pont entre la modélisation conceptuelle et l’implémentation. Le **diagramme de séquence** précise, pour chaque cas d’utilisation, la chronologie des messages échangés entre les acteurs et les composants ; il est particulièrement utile pour décrire les interactions temps réel telles que la signalisation WebRTC. Enfin, un **diagramme de déploiement** pourra être utilisé pour représenter la répartition physique des composants entre les postes clients, le serveur Spring Boot et les serveurs STUN/TURN.

L’usage combiné de ces quatre diagrammes garantit une vision complète du système : ce que l’utilisateur attend, comment les objets sont structurés, comment ils interagissent dans le temps et où ils s’exécutent.

---

# CHAPITRE 2 : PLANIFICATION ET SPÉCIFICATION DES BESOINS

## 2.1 Analyse des Besoins

L’analyse des besoins constitue la pierre angulaire de la conception logicielle. Elle traduit les attentes exprimées par les utilisateurs en exigences claires, vérifiables et hiérarchisées. Pour un projet à forte composante temps réel comme une plateforme de support à distance, cette étape revêt une importance particulière, car elle conditionne directement la qualité de l’expérience perçue par l’utilisateur final.

### 2.1.1 Identification des Acteurs

Six acteurs principaux interviennent dans le fonctionnement de la plateforme. Aucun d’entre eux ne joue le rôle d’administrateur dédié, dans la mesure où la présente version du projet repose sur une architecture entièrement client‑to‑client.

**1. Utilisateur demandeur d’assistance.** Il s’agit de l’utilisateur qui rencontre un dysfonctionnement sur son poste et qui souhaite recevoir de l’aide à distance. Il lance l’application desktop, s’authentifie auprès du serveur central, puis accepte ou refuse les demandes de connexion qui lui sont adressées. Il conserve à tout instant la maîtrise des permissions accordées au cours de la session : il peut autoriser ou refuser le contrôle clavier/souris, le transfert de fichiers ou l’utilisation de l’IA, et peut également mettre fin à la session de son propre chef.

**2. Utilisateur assistant ou technicien.** Cet acteur correspond à la personne qui apporte son aide à distance. Il utilise la même application desktop que le demandeur, mais dans un rôle inverse. Il peut consulter la liste des clients connectés, formuler une demande de session vers un poste cible, visualiser l’écran distant après acceptation, exécuter des actions clavier/souris dans la limite des autorisations qu’il a reçues, échanger des messages dans le chat, transférer des fichiers et solliciter l’assistant IA pour appuyer son diagnostic.

**3. Application desktop client.** L’application installée sur chaque poste joue le rôle d’acteur logiciel à part entière. Elle assure la capture d’écran, l’encodage vidéo, la transmission temps réel du flux, la réception et la simulation des entrées utilisateur, le transfert de fichiers, l’affichage du chat, la remontée des métriques système et la communication avec le serveur central. Cette application est strictement identique sur l’ensemble des postes : c’est le contexte de la session qui détermine si elle se comporte comme cliente émettrice ou réceptrice.

**4. Serveur backend central / serveur de signalisation.** Il s’agit du serveur Spring Boot. Il assume plusieurs responsabilités, qui couvrent l’ensemble du plan de contrôle de la plateforme : authentification des utilisateurs et délivrance des jetons JWT, gestion des comptes et des profils, enregistrement et suivi des clients connectés via WebSocket, création des sessions de contrôle, gestion des autorisations, persistance des messages de chat, journalisation des transferts de fichiers, persistance des sessions IA et, surtout, relais des messages de signalisation WebRTC (SDP Offer/Answer et candidats ICE) entre les deux clients impliqués dans une session.

**5. Système IA.** L’assistant IA intervient ponctuellement, à la demande de l’utilisateur assistant. Il reçoit une capture d’écran de la machine distante, l’analyse à travers un modèle multimodal (l’API Gemini dans la présente version), et renvoie une suggestion structurée : description du contexte observé, diagnostic probable et, le cas échéant, séquence d’actions exécutables. Ces actions sont soumises à la validation explicite de l’utilisateur avant exécution.

**6. Serveurs STUN/TURN.** Ces serveurs jouent un rôle d’infrastructure pour la connectivité WebRTC. Le serveur **STUN** permet à chaque client de découvrir son adresse publique vue depuis Internet. Le serveur **TURN** sert de relais média lorsque la connexion directe entre les deux clients est impossible, par exemple dans le cas de NAT symétriques ou de pare‑feux restrictifs. Sans cette infrastructure, l’établissement d’une connexion pair à pair fiable entre deux postes situés sur des réseaux différents serait, dans bien des cas, irréalisable.

### 2.1.2 Besoins Fonctionnels

Les besoins fonctionnels décrivent les services que la plateforme doit rendre à ses utilisateurs. Ils ont été collectés à partir des objectifs énoncés par l’encadrant, complétés par une étude de l’existant et formalisés au cours des premiers sprints du projet.

La plateforme doit, en premier lieu, permettre l’**authentification** des utilisateurs auprès du serveur central. Cette authentification produit un jeton JWT, utilisé par la suite pour toutes les requêtes REST et pour la connexion WebSocket. Elle doit, en deuxième lieu, supporter l’**installation à l’identique** de l’application desktop sur tous les postes utilisateurs, sans qu’aucune installation spécifique ne distingue le rôle d’assistant de celui de demandeur. Une fois l’application installée et l’utilisateur authentifié, le client doit s’**enregistrer automatiquement** auprès du serveur en ouvrant un canal WebSocket persistant. Cette présence est rafraîchie périodiquement par des messages de *heartbeat*, ce qui permet à l’ensemble du système de connaître à tout instant les postes disponibles.

L’interface client doit afficher de manière permanente l’**état de connexion** de l’application et l’identifiant unique du poste. Lorsqu’un utilisateur rencontre un problème, il peut **créer une demande de session d’assistance** vers un autre poste. L’utilisateur distant doit alors pouvoir **accepter ou refuser** cette demande, et **définir, au moment de l’acceptation, les permissions** qu’il accorde à l’assistant : contrôle clavier/souris, transfert de fichiers, autorisation d’utilisation de l’IA.

Sur le plan temps réel, la plateforme doit **établir une connexion WebRTC** entre les deux clients. Cette connexion repose sur l’**échange de messages SDP Offer/Answer**, transmis par l’intermédiaire du serveur backend central via WebSocket. Elle est complétée par l’**échange des candidats ICE** entre les deux pairs, et par l’utilisation de **serveurs STUN/TURN** pour traverser les éventuels NAT. Une fois la connexion établie, l’application doit assurer le **partage d’écran en temps réel**, avec **encodage vidéo H.264**, le **contrôle clavier/souris à distance**, le **chat en temps réel** entre les deux utilisateurs ainsi que le **transfert de fichiers** pair à pair.

Au‑delà des fonctionnalités strictes de prise en main, la plateforme doit permettre la **consultation des métriques système** du poste distant (utilisation CPU, RAM et espace disque), et offrir l’accès à un **assistant IA** capable d’analyser une capture d’écran et de proposer des actions. Enfin, l’ensemble du cycle de vie d’une session doit pouvoir se **terminer proprement**, avec **journalisation** des événements importants : ouverture de session, acceptation, exécution d’actions IA, transferts de fichiers, fermeture.

Sous forme synthétique, les besoins fonctionnels du système sont les suivants :

- Authentification des utilisateurs auprès du serveur central.
- Installation à l’identique de l’application desktop sur l’ensemble des postes.
- Enregistrement automatique du client auprès du serveur dès l’ouverture de session.
- Affichage en continu de l’état de connexion de l’application.
- Création d’une demande de session d’assistance vers un poste cible.
- Acceptation ou refus de la session par l’utilisateur distant.
- Gestion fine des permissions associées à chaque session.
- Autorisation ou refus du contrôle clavier et souris.
- Autorisation ou refus du transfert de fichiers.
- Établissement d’une connexion WebRTC pair à pair.
- Échange SDP Offer/Answer via le serveur backend central.
- Échange des candidats ICE via WebSocket.
- Recours aux serveurs STUN/TURN pour la traversée des NAT.
- Partage d’écran en temps réel.
- Encodage vidéo H.264 du flux d’écran.
- Contrôle clavier et souris à distance.
- Chat en temps réel entre les deux utilisateurs.
- Transfert de fichiers entre les deux clients.
- Consultation des métriques CPU, RAM et disque.
- Utilisation de l’assistant IA pour le diagnostic.
- Fermeture propre de la session par l’un ou l’autre des utilisateurs.
- Journalisation des événements importants liés à la session.

### 2.1.3 Besoins Non Fonctionnels

Les besoins non fonctionnels désignent les qualités attendues du système, qui conditionnent son acceptabilité au‑delà des fonctionnalités proprement dites. Pour une plateforme de prise en main à distance, ces exigences revêtent un poids particulièrement important, car elles touchent à la fois à la confiance des utilisateurs, à la performance perçue et à la conformité avec les politiques de sécurité.

La **sécurité** est la première préoccupation. Toute communication avec le serveur central doit emprunter HTTPS, et les WebSockets doivent être protégés par WSS en production. L’authentification repose sur des jetons JWT signés et expirables ; les mots de passe sont stockés sous forme hachée. Les flux WebRTC bénéficient quant à eux du chiffrement de bout en bout assuré nativement par le protocole. La **confidentialité** complète cette exigence : aucune capture d’écran ne doit être conservée par défaut sur le serveur, et les communications entre clients ne transitent jamais par le serveur central, à l’exception des messages de signalisation.

La **performance** et la **faible latence** sont essentielles à l’usage temps réel. Le système doit pouvoir maintenir un flux vidéo fluide, idéalement sous le seuil des 150 millisecondes de latence perçue dans des conditions réseau correctes, et garantir la réactivité des entrées clavier/souris. La **fiabilité** s’exprime à travers la capacité du système à maintenir des sessions stables pendant plusieurs dizaines de minutes sans interruption ; la **robustesse face aux coupures réseau** se traduit par des mécanismes de reconnexion automatique du WebSocket et par la résilience du pipeline WebRTC.

La **maintenabilité** et l’**évolutivité** sont assurées par une architecture modulaire : chaque domaine fonctionnel (authentification, session, signaling, agent, chat, transfert de fichiers, IA) est isolé dans son propre module Spring Boot, et le code Rust est lui aussi structuré par responsabilités. L’**ergonomie** de l’interface, conçue avec Svelte, vise à rendre l’application accessible à des utilisateurs non spécialistes : les états sont signalés visuellement, les permissions sont explicites et les actions critiques font l’objet d’une confirmation. La **disponibilité** du serveur central, enfin, doit être suffisamment élevée pour garantir la prise en charge des demandes au moment où elles surviennent.

Plusieurs autres qualités structurent l’ingénierie du projet. La **compatibilité Windows** est exigée pour la version actuelle, le système d’exploitation cible étant Windows 10 et au‑delà ; la **traçabilité** des événements est assurée par la journalisation côté serveur ainsi que par la persistance des sessions, des messages et des transferts ; la **protection contre les actions dangereuses de l’IA** s’appuie sur une validation explicite par l’utilisateur avant toute exécution ; la **simplicité d’installation et d’utilisation** se concrétise par un installeur unique et une interface dépouillée ; et la **stabilité de la connexion WebRTC** repose sur une configuration soignée des serveurs STUN/TURN ainsi que sur une gestion défensive des changements d’état du pair distant.

## 2.2 Pilotage du Projet avec Scrum

### 2.2.1 Identification de l’Équipe Scrum

L’équipe Scrum mise en place pour ce projet de fin d’études adapte le cadre méthodologique classique aux contraintes d’un travail individuel encadré. Le **Product Owner** est incarné par l’encadrant professionnel ; il porte la vision du produit, hiérarchise les éléments du backlog et arbitre les compromis fonctionnels. Le **Scrum Master**, dans le contexte académique, est assuré conjointement par l’étudiant et son encadrant pédagogique ; il veille au respect des cérémonies, à la levée des obstacles et à la cohésion méthodologique du projet. L’**équipe de développement** est composée du seul étudiant, qui assume l’intégralité des activités de conception, d’implémentation et de test. À leurs côtés, l’**encadrant académique** garantit le cadrage scientifique du projet, tandis que l’**encadrant professionnel** assure la conformité avec les attentes de l’organisme d’accueil. Cette organisation, bien que ramassée, respecte l’esprit de Scrum en assurant des responsabilités claires et un point de vérification régulier sur la valeur produite.

### 2.2.2 Backlog du Produit

Le backlog produit regroupe l’ensemble des fonctionnalités souhaitées, formulées sous la forme de user stories, hiérarchisées et associées à un critère d’acceptation vérifiable. Le tableau ci‑dessous présente les principales user stories retenues pour le projet.

| ID  | User Story | Priorité | Critère d’acceptation |
|-----|------------|----------|------------------------|
| US01 | En tant qu’utilisateur, je veux m’authentifier afin d’accéder à l’application. | Haute | L’utilisateur obtient un jeton JWT valide après saisie d’identifiants corrects. |
| US02 | En tant qu’utilisateur, je veux lancer l’application desktop afin de rendre mon poste disponible. | Haute | L’application s’ouvre, s’enregistre auprès du serveur et affiche l’état « connecté ». |
| US03 | En tant qu’utilisateur demandeur d’assistance, je veux créer une demande d’aide à distance. | Haute | Une session est créée côté serveur et une notification est envoyée au poste cible. |
| US04 | En tant qu’utilisateur distant, je veux accepter ou refuser une session. | Haute | La décision est transmise au demandeur et le statut de la session est mis à jour. |
| US05 | En tant qu’utilisateur distant, je veux choisir les permissions accordées. | Haute | Les permissions sélectionnées sont persistées et appliquées côté assistant. |
| US06 | En tant qu’utilisateur assistant, je veux visualiser l’écran distant. | Haute | Un flux vidéo H.264 est affiché en temps réel sur le poste assistant. |
| US07 | En tant qu’utilisateur assistant, je veux contrôler la souris et le clavier si l’autorisation est accordée. | Haute | Les entrées émises sont reproduites sur le poste distant avec une latence acceptable. |
| US08 | En tant qu’utilisateur, je veux échanger des messages via un chat. | Moyenne | Les messages sont transmis en temps réel et persistés côté serveur. |
| US09 | En tant qu’utilisateur, je veux envoyer et recevoir des fichiers. | Moyenne | Un fichier transféré arrive intact sur la machine cible et apparaît dans l’historique. |
| US10 | En tant que système, je veux établir une connexion WebRTC entre deux clients. | Haute | Les SDP et candidats ICE sont échangés et l’état ICE atteint « connected ». |
| US11 | En tant que système, je veux collecter les métriques CPU/RAM/DISQUE. | Moyenne | Les métriques sont remontées périodiquement et stockées en base. |
| US12 | En tant qu’utilisateur assistant, je veux utiliser l’IA pour m’aider au diagnostic. | Moyenne | L’IA produit une suggestion structurée à partir d’une capture d’écran fournie. |
| US13 | En tant que système, je veux gérer les coupures de connexion. | Haute | La reconnexion WebSocket est automatique et la session est restaurée si possible. |
| US14 | En tant qu’utilisateur, je veux terminer proprement une session. | Haute | La session est fermée des deux côtés et son statut final est journalisé. |

### 2.2.3 Planification des Sprints

La réalisation du projet, étalée sur six mois de stage, a été découpée en huit sprints d’une durée approximative de deux semaines chacun. Cette planification permet d’avancer par paliers cohérents, en livrant à chaque itération un incrément démontrable.

**Sprint 1 — Analyse, conception et préparation de l’environnement.** Objectif : poser les fondations du projet. Tâches : étude de l’existant, formalisation des besoins, choix des technologies, mise en place des dépôts Git, initialisation des projets Tauri/Rust et Spring Boot. Livrables : documents d’analyse, premières maquettes, environnement de développement opérationnel.

**Sprint 2 — Authentification et gestion des clients connectés.** Objectif : permettre aux utilisateurs de s’authentifier et aux clients de s’enregistrer. Tâches : modélisation des entités `User` et `Agent`, mise en place du module `auth` (JWT, hachage des mots de passe), implémentation du WebSocket `agent` côté serveur et du gestionnaire d’enregistrement côté client. Livrables : flux d’authentification complet, présence des clients visible côté serveur.

**Sprint 3 — Gestion des sessions et signalisation WebSocket intégrée au backend.** Objectif : créer les sessions et acheminer les messages de signalisation. Tâches : entité `ControlSession`, contrôleur REST de sessions, WebSocket de signalisation, types de messages SDP/ICE, intégration côté client via le `signal-bus`. Livrables : ouverture et acceptation de session fonctionnelles, signalisation prête pour WebRTC.

**Sprint 4 — Intégration WebRTC et partage d’écran.** Objectif : produire le flux vidéo temps réel. Tâches : pipeline de capture DXGI Desktop Duplication, encodage H.264, transport via WebRTC, configuration STUN/TURN, intégration côté assistant. Livrables : premier flux d’écran consultable depuis le poste assistant.

**Sprint 5 — Contrôle clavier/souris et chat.** Objectif : permettre l’interaction distante. Tâches : capture des événements côté assistant, transmission via DataChannel, injection des entrées côté distant, mise en œuvre du chat temps réel. Livrables : contrôle clavier/souris fonctionnel, chat texte intégré.

**Sprint 6 — Transfert de fichiers et métriques système.** Objectif : enrichir l’outil de support. Tâches : DataChannel dédié au transfert de fichiers, historique des transferts, collecteur de métriques CPU/RAM/disque, persistance des métriques. Livrables : transferts de fichiers stables, tableau de bord de métriques.

**Sprint 7 — Intégration de l’IA.** Objectif : ajouter la couche d’assistance intelligente. Tâches : pipeline d’envoi de capture d’écran, appel à l’API Gemini, structuration des réponses en actions, validation utilisateur avant exécution. Livrables : assistant IA fonctionnel pour le diagnostic.

**Sprint 8 — Tests, optimisation et finalisation.** Objectif : stabiliser et livrer la version finale. Tâches : tests de bout en bout, optimisation de la latence vidéo, durcissement de la sécurité, packaging de l’installeur, rédaction du rapport. Livrables : application stable, documentation, rapport PFE.

## 2.3 Diagramme de Cas d’Utilisation Généralisé

Le diagramme de cas d’utilisation généralisé représente, au plus haut niveau d’abstraction, l’ensemble des fonctionnalités offertes par la plateforme et les interactions de chacun des acteurs avec ces fonctionnalités. Sa lecture permet de saisir d’un coup d’œil le périmètre fonctionnel du système.

Les acteurs identifiés sont au nombre de cinq pour ce diagramme global : l’**utilisateur demandeur d’assistance**, l’**utilisateur assistant**, le **serveur backend central / serveur de signalisation**, le **système IA** et les **serveurs STUN/TURN**. Les deux premiers sont des acteurs humains, les trois autres sont des acteurs logiciels ou des composants d’infrastructure.

Les cas d’utilisation retenus pour ce diagramme couvrent l’intégralité du cycle de vie d’une session de support. Ils comprennent : *s’authentifier*, *lancer l’application client*, *enregistrer le client auprès du serveur*, *demander une session d’assistance*, *accepter ou refuser une session*, *définir les permissions de session*, *partager l’écran*, *contrôler la machine distante*, *transférer des fichiers*, *échanger via chat*, *consulter les métriques système*, *utiliser l’assistant IA* et *terminer la session*.

Les interactions s’organisent comme suit. Le demandeur d’assistance et l’assistant participent l’un et l’autre à l’authentification, au lancement de l’application et à son enregistrement auprès du serveur. Le demandeur est associé aux cas d’utilisation d’acceptation de session et de définition des permissions, tandis que l’assistant est l’acteur principal de la demande de session, du contrôle distant, du partage d’écran consulté, du chat, du transfert de fichiers, de la consultation des métriques et de l’utilisation de l’IA. Le serveur backend central est rattaché à l’authentification, à l’enregistrement des clients, à la gestion des sessions, au suivi de l’état des clients et à l’échange des messages SDP Offer/Answer et des candidats ICE qu’il relaie via WebSocket. Le système IA participe au cas d’utilisation *utiliser l’assistant IA*. Les serveurs STUN/TURN, enfin, sont associés à l’établissement de la connexion WebRTC, en tant que support de la découverte d’adresses publiques et de relais média éventuel.

Plusieurs relations d’**include** structurent le diagramme. Le cas *demander une session d’assistance* inclut *définir les permissions de session* lors de l’acceptation. Les cas *partager l’écran*, *contrôler la machine distante*, *transférer des fichiers* et *échanger via chat* incluent tous, en pratique, un échange préalable de signalisation WebRTC avec le serveur backend central. Le cas *utiliser l’assistant IA* dépend d’une session active et d’un flux vidéo disponible. Cette structure d’inclusion traduit la dépendance forte entre les fonctionnalités temps réel et la couche de signalisation.

## 2.4 Diagramme de Classes Global

Le diagramme de classes global formalise la structure statique du système. Il reflète d’une part les entités persistées en base de données — soit les tables `users`, `agents`, `control_sessions`, `agent_metrics`, `chat_messages`, `file_transfer_logs` et `ai_sessions` — et d’autre part les composants techniques qui structurent la communication temps réel.

**Classe `User`.** Elle représente un utilisateur authentifié de la plateforme. Ses principaux attributs sont l’identifiant, l’adresse électronique, le nom complet, le mot de passe haché, le rôle technique (au sens des droits d’accès au backend, et non du rôle de session), la date de création et la date de dernière connexion. Ses méthodes essentielles couvrent l’authentification (`authenticate`), la mise à jour des informations de profil (`updateProfile`) et la gestion du mot de passe (`changePassword`). Un utilisateur peut, selon le contexte d’une session donnée, prendre le rôle de demandeur d’assistance ou d’assistant ; ce rôle n’est pas une propriété figée de la classe, mais résulte de l’association `ControlSession.requester` ou `ControlSession.helper`.

**Classe `Agent`.** Elle représente l’application cliente installée sur un poste. Ses attributs principaux sont l’identifiant de l’agent, le nom de la machine, la plateforme, la version de l’application, l’état de présence (en ligne, hors ligne, occupé) et l’horodatage du dernier *heartbeat*. Les méthodes typiques sont `connect`, `disconnect`, `sendHeartbeat` et `pushMetrics`. Chaque agent est rattaché à un utilisateur par une association `Agent.owner → User`.

**Classe `ControlSession`.** Elle modélise une session de contrôle à distance entre deux clients. Ses attributs sont l’identifiant de session, la référence au demandeur, la référence à l’assistant, le statut (demandée, acceptée, en cours, terminée, refusée, expirée), les permissions accordées, l’horodatage de création, d’acceptation et de fermeture, ainsi que la raison de fermeture. Ses méthodes principales sont `request`, `approve`, `reject`, `start`, `end` et `updatePermissions`. Une session est associée à exactement deux utilisateurs et peut être liée à de nombreux objets `ChatMessage`, `FileTransferLog`, `AgentMetrics` (mesures contextuelles), `AiSession` et `SignalMessage`.

**Classe `AgentMetrics`.** Elle représente une mesure des ressources système prélevée sur un agent à un instant donné. Ses attributs typiques sont l’identifiant de mesure, l’agent concerné, l’utilisation CPU, l’utilisation mémoire, l’utilisation disque, l’horodatage et, le cas échéant, la session active au moment de la mesure. La méthode principale est `record`. Elle est liée à `Agent` par une association multiple.

**Classe `ChatMessage`.** Elle représente un message échangé pendant une session. Ses attributs sont l’identifiant, la session, l’émetteur, le destinataire, le contenu, l’horodatage et le statut de livraison. Les méthodes typiques sont `send`, `markAsDelivered` et `markAsRead`. Elle est rattachée à `ControlSession` et à deux références `User` (émetteur et destinataire).

**Classe `FileTransferLog`.** Elle représente une entrée d’historique des transferts de fichiers. Ses attributs comprennent l’identifiant, la session, l’expéditeur, le destinataire, le nom du fichier, sa taille, sa direction (envoi/réception), son statut (en cours, terminé, échec, annulé), l’horodatage de début et de fin. Les méthodes essentielles sont `start`, `updateProgress`, `complete` et `fail`. Elle est associée à `ControlSession` et à deux références `User`.

**Classe `AiSession`.** Elle représente l’ensemble des interactions liées à l’assistant IA au cours d’une session de contrôle. Ses attributs principaux sont l’identifiant, la session, l’utilisateur initiateur, la description du contexte, la requête textuelle envoyée à l’IA, la réponse structurée reçue, les actions exécutées et l’horodatage. Ses méthodes sont `analyzeFrame`, `proposeActions`, `executeApprovedActions` et `cancel`.

**Classe `SignalMessage`.** Elle modélise les messages de signalisation WebRTC transportés par le serveur backend central. Ses attributs sont l’identifiant logique, la session, l’émetteur, le destinataire, le type (`offer`, `answer`, `ice-candidate`, `bye`), la charge utile SDP ou ICE et l’horodatage. Ses méthodes sont `route` et `deliver`. Elle est intrinsèquement liée au serveur backend, qui en assure le routage entre les deux clients impliqués.

**Classe `Permission`.** Elle représente les autorisations accordées au cours d’une session. Ses attributs distinguent notamment le contrôle clavier/souris, le transfert de fichiers et l’utilisation de l’IA. Ses méthodes principales sont `grant`, `revoke` et `isAllowed`. Elle est associée à `ControlSession` par une relation de composition.

**Classe `WebRtcConnection`.** Elle représente la connexion temps réel pair à pair entre deux clients. Ses attributs sont la session associée, l’état ICE, l’état DTLS, le débit instantané, la latence mesurée et la liste des DataChannels ouverts. Ses méthodes principales sont `open`, `close`, `addIceCandidate` et `getStats`.

**Classe `IceCandidate`.** Elle représente un candidat ICE échangé pour établir la connexion. Ses attributs sont l’adresse, le port, le protocole, le type (host, srflx, relay) et la priorité. Sa méthode principale est `serialize`. Elle est rattachée à `WebRtcConnection` et `SignalMessage`.

**Classe `AuditLog`.** Elle représente la journalisation des événements importants : ouverture et fermeture de session, modifications de permissions, transferts de fichiers, actions de l’IA, déconnexions inattendues. Ses attributs sont l’horodatage, l’utilisateur concerné, le type d’événement, la cible et un payload descriptif. Sa méthode principale est `record`.

L’ensemble s’articule autour de deux pivots structurants. Le premier est la classe `User`, qui concentre l’identité numérique et autour de laquelle se construisent toutes les associations métier. Le second est la classe `ControlSession`, qui sert de conteneur logique pour toutes les interactions temps réel : c’est elle qui agrège les messages, les transferts, les métriques contextuelles, les sessions IA et les messages de signalisation. Ce double pivotement reflète fidèlement la nature *client‑to‑client* de la plateforme, où la session est le véritable point d’ancrage de l’expérience, tandis que l’utilisateur n’est pas attaché à un rôle figé.

## 2.5 Environnement de Travail

### 2.5.1 Environnement Matériel

L’environnement matériel rassemble les ressources physiques mobilisées pour le développement, les tests et la validation de la plateforme. Un **poste de développement** principal, équipé de Windows 10 et muni de capacités suffisantes pour exécuter simultanément l’IDE, le compilateur Rust, le runtime Java et les outils de débogage, constitue la pièce centrale du dispositif. **Deux machines clientes Windows**, au minimum, ont été mises à contribution pour tester les sessions de contrôle ; elles ont été utilisées dans des configurations réseau variées afin de valider la robustesse de la connexion WebRTC. Une **connexion Internet** stable et un **routeur** local ont permis d’expérimenter le comportement de la solution derrière un NAT classique. Un **serveur distant** hébergé dans le cloud a été utilisé pour exécuter le backend Spring Boot et exposer le service à des clients situés sur des **réseaux différents**, ce qui a permis de valider l’efficacité des serveurs STUN et TURN dans des conditions réalistes.

### 2.5.2 Environnement Logiciel et Développement

L’environnement logiciel s’organise autour d’une pile cohérente, sélectionnée pour son adéquation aux exigences temps réel et à la nature native de l’application desktop.

- **Rust** est le langage utilisé pour la logique native de l’application desktop. Il offre des performances proches du C++ tout en garantissant la sûreté mémoire et la concurrence sans course critique.
- **Tauri** est le *framework* retenu pour la couche d’interface desktop. Il combine un *runtime* natif léger avec un *frontend* web, ce qui permet de produire des binaires de petite taille et à faible empreinte mémoire.
- **Svelte** est utilisé comme bibliothèque d’interface utilisateur ; sa compilation en JavaScript minimal et son modèle de réactivité explicite conviennent particulièrement à une application temps réel.
- **Spring Boot** est utilisé pour le backend central. Il fournit l’écosystème nécessaire à la gestion des contrôleurs REST, des WebSockets, de la persistance et de la sécurité.
- **WebRTC** est le protocole de transport temps réel. Il prend en charge la négociation des médias, le chiffrement de bout en bout et la traversée des NAT.
- **WebSocket** est le protocole d’échange persistant entre clients et serveur, utilisé pour la signalisation et pour la remontée d’informations en temps réel.
- **MySQL** est le système de gestion de base de données relationnelle utilisé pour persister les utilisateurs, les agents, les sessions, les messages, les transferts, les métriques et les sessions IA.
- **JWT** sert de mécanisme d’authentification entre clients et serveur.
- **STUN/TURN** complètent le protocole WebRTC pour la traversée des NAT et le relais des flux médias.
- **Git** et **GitHub** assurent le versionnement et la collaboration.
- **Visual Studio Code** est l’éditeur principal, complété par les extensions Rust, Java, Svelte et Tauri.
- **Postman** est utilisé pour tester manuellement les API REST.
- **Render** et **Railway** sont les plateformes cloud envisagées pour le déploiement du backend.
- **L’API Gemini** fournit le moteur multimodal utilisé pour l’assistance IA.
- **Windows** est le système d’exploitation cible des clients.
- Un **navigateur web** sert également de surface auxiliaire de test et d’inspection.

---

# CHAPITRE 3 : SPRINT 1

Le premier sprint du projet est consacré à la mise en place des fondations méthodologiques, conceptuelles et techniques. Il vise à transformer la vision initiale en une base de travail concrète, comprenant un environnement opérationnel, une architecture cible documentée et une première itération d’interfaces utilisateurs.

## 3.1 Backlog du Sprint 1

Le backlog du Sprint 1 rassemble les user stories sélectionnées pour cette première itération. Elles concernent l’analyse du besoin, la préparation de l’environnement, la conception initiale, la définition de l’architecture générale, la mise en place du projet, la définition des premiers modules et la clarification du fonctionnement client‑to‑client.

| ID    | User Story | Tâches | Priorité | État |
|-------|------------|--------|----------|------|
| S1‑01 | En tant qu’équipe projet, je veux clarifier le besoin du client. | Réunions de cadrage, analyse de l’existant, rédaction du périmètre fonctionnel. | Haute | Terminée |
| S1‑02 | En tant que développeur, je veux préparer mon environnement de développement. | Installation des SDK Rust, Java, Node.js, configuration de l’IDE, mise en place des dépôts Git. | Haute | Terminée |
| S1‑03 | En tant qu’architecte, je veux définir l’architecture client‑to‑client. | Définition des composants, diagrammes d’architecture, choix des protocoles. | Haute | Terminée |
| S1‑04 | En tant qu’équipe projet, je veux modéliser les besoins. | Diagrammes UML de cas d’utilisation et de classes, formalisation des acteurs. | Haute | Terminée |
| S1‑05 | En tant qu’architecte, je veux définir les premiers modules backend. | Découpage en modules `user`, `agent`, `session`, `signaling`, `chat`, `filetransfer`, `ai`. | Haute | Terminée |
| S1‑06 | En tant que développeur, je veux poser les bases du projet Tauri/Rust. | Initialisation du projet, configuration, structuration des dossiers `agent` et `lib`. | Haute | Terminée |
| S1‑07 | En tant qu’utilisateur, je veux m’authentifier. | Conception de la page de connexion, intégration du backend d’authentification JWT. | Haute | Terminée |
| S1‑08 | En tant qu’utilisateur, je veux lancer l’application client. | Conception de l’écran principal, gestion de l’état de connexion, premier enregistrement auprès du serveur. | Haute | Terminée |
| S1‑09 | En tant qu’utilisateur demandeur, je veux demander une session d’assistance. | Conception du flux REST de création de session, design de la liste des clients. | Haute | Terminée |
| S1‑10 | En tant qu’utilisateur distant, je veux accepter ou refuser une session. | Conception du modal d’approbation, gestion des permissions à l’acceptation. | Haute | Terminée |

## 3.2 Spécification Fonctionnelle

### 3.2.1 Description textuelle du cas d’utilisation « S’authentifier »

- **Acteur principal** : utilisateur (demandeur d’assistance ou assistant, sans distinction à ce stade).
- **Acteurs secondaires** : serveur backend central.
- **Objectif** : permettre à un utilisateur de prouver son identité afin d’accéder aux fonctionnalités de la plateforme.
- **Préconditions** : l’utilisateur possède un compte valide, l’application desktop est installée sur le poste, le serveur backend est accessible.
- **Scénario nominal** :
  1. L’utilisateur lance l’application desktop.
  2. L’application affiche l’écran d’authentification, composé d’un champ adresse électronique, d’un champ mot de passe et d’un bouton de connexion.
  3. L’utilisateur saisit ses identifiants et valide.
  4. L’application transmet la requête au serveur central via une requête HTTPS sur l’endpoint d’authentification.
  5. Le serveur vérifie les identifiants, génère un jeton JWT et renvoie une réponse positive.
  6. L’application stocke le jeton de manière sécurisée et affiche l’écran principal.
- **Scénarios alternatifs** :
  - *Identifiants invalides* : le serveur renvoie une erreur 401, l’application affiche un message d’erreur localisé sans révéler la cause exacte.
  - *Serveur inaccessible* : l’application affiche un message indiquant qu’elle ne peut joindre le service et propose une nouvelle tentative.
  - *Compte verrouillé* : le serveur renvoie une erreur explicite, l’application invite l’utilisateur à contacter le support.
- **Postconditions** : un jeton JWT valide est disponible côté client et l’application bascule sur l’écran principal en état authentifié.

### 3.2.2 Description textuelle du cas d’utilisation « Lancer l’application client »

- **Acteur principal** : utilisateur authentifié.
- **Acteurs secondaires** : serveur backend central, serveur de signalisation (intégré au backend).
- **Objectif** : démarrer l’application desktop, signaler la présence du poste au serveur et rendre le client disponible pour les interactions ultérieures.
- **Préconditions** : l’utilisateur est authentifié et dispose d’un jeton JWT valide ; la connexion Internet est opérationnelle.
- **Scénario nominal** :
  1. L’application initialise ses composants internes : agent local, gestionnaire de signal, gestionnaire d’approbation et pipeline IA.
  2. L’application ouvre une connexion WebSocket vers le serveur en présentant son jeton JWT.
  3. Le serveur valide le jeton, enregistre la présence du client et lui attribue un identifiant d’agent.
  4. L’application affiche l’écran principal avec l’état « connecté » et l’identifiant du poste.
  5. L’application démarre un *heartbeat* périodique vers le serveur.
- **Scénarios alternatifs** :
  - *Jeton expiré* : le serveur refuse la connexion WebSocket, l’application invite l’utilisateur à se réauthentifier.
  - *Coupure réseau* : l’application bascule en état « hors ligne » et tente de se reconnecter à intervalles croissants.
- **Postconditions** : le client est enregistré et visible côté serveur, prêt à émettre ou recevoir des demandes de session.

### 3.2.3 Cas d’utilisation : « Demander une session d’assistance »

- **Acteur principal** : utilisateur dans le rôle de demandeur d’assistance.
- **Acteurs secondaires** : serveur backend central, utilisateur distant cible.
- **Objectif** : initier une session de contrôle à distance vers un poste cible identifié.
- **Préconditions** : les deux utilisateurs sont authentifiés ; le poste cible est en ligne ; le demandeur connaît l’identifiant du poste à contacter.
- **Scénario nominal** :
  1. Le demandeur sélectionne, dans son interface, un poste cible visible.
  2. L’application envoie au serveur une requête REST de création de session, contenant les références du demandeur et du destinataire.
  3. Le serveur enregistre une nouvelle entrée `ControlSession` avec le statut « demandée » et notifie le poste cible via WebSocket.
  4. Le poste cible affiche une notification d’approbation.
- **Scénarios alternatifs** :
  - *Poste cible hors ligne* : le serveur retourne une erreur explicite, le demandeur est invité à réessayer plus tard.
  - *Session déjà en cours sur la cible* : la nouvelle demande est mise en file ou refusée selon la politique configurée.
- **Postconditions** : une session est créée côté serveur avec le statut « demandée » et le poste cible est sollicité pour décision.

### 3.2.4 Cas d’utilisation : « Accepter ou refuser une session »

- **Acteur principal** : utilisateur distant sollicité.
- **Acteurs secondaires** : serveur backend central, demandeur d’assistance.
- **Objectif** : permettre à l’utilisateur distant de statuer sur une demande de session et, le cas échéant, d’en définir les permissions.
- **Préconditions** : une demande de session est en attente sur le poste distant.
- **Scénario nominal** :
  1. L’application affiche un modal d’approbation indiquant l’identité du demandeur et la nature de la demande.
  2. L’utilisateur distant sélectionne les permissions qu’il souhaite accorder : contrôle clavier/souris, transfert de fichiers, utilisation de l’IA.
  3. L’utilisateur clique sur « Accepter ».
  4. L’application transmet la décision et les permissions au serveur, qui met à jour l’entité `ControlSession` et notifie le demandeur.
  5. Les deux parties initient alors la signalisation WebRTC.
- **Scénarios alternatifs** :
  - *Refus* : l’utilisateur clique sur « Refuser ». Le serveur met la session à l’état « refusée » et notifie le demandeur ; la session se termine immédiatement.
  - *Délai d’expiration* : si aucune décision n’est prise dans le délai imparti, la session passe à l’état « expirée ».
- **Postconditions** : la session passe au statut « acceptée » ou « refusée », et les permissions sont enregistrées pour la suite de la session.

## 3.3 Diagramme de Séquence

Plusieurs diagrammes de séquence sont produits dans le cadre du Sprint 1 pour décrire la chronologie des messages échangés entre les composants.

**Diagramme de séquence d’authentification.** Les participants sont l’utilisateur, l’interface desktop, le contrôleur d’authentification du serveur et la base de données. La séquence commence par la saisie des identifiants par l’utilisateur. L’interface émet une requête HTTP `POST /api/auth/login`. Le contrôleur vérifie le mot de passe via le service utilisateur, qui interroge la base, puis génère un JWT et renvoie la réponse. L’interface stocke le jeton et passe à l’écran principal.

**Diagramme de séquence de lancement et d’enregistrement du client.** Les participants sont l’application desktop, le `WebSocket Agent` du serveur et la couche de présence. À l’ouverture, l’application établit une connexion WebSocket avec le jeton dans les en‑têtes. Le serveur valide le jeton, instancie ou récupère l’entité `Agent`, l’associe à la connexion et envoie un accusé de réception. L’application démarre alors le *heartbeat* périodique, qui rafraîchit l’horodatage de présence côté serveur.

**Diagramme de séquence de demande de session.** Les participants sont le demandeur, l’interface client, le contrôleur REST des sessions, le service de session, le WebSocket Agent et le poste distant. Le demandeur déclenche la création d’une session via un `POST /api/sessions`. Le service crée l’entité `ControlSession` avec le statut « demandée » et délègue au WebSocket Agent l’envoi d’une notification de type `session-request` au destinataire. Le poste distant affiche le modal d’approbation et l’interaction se poursuit dans le diagramme suivant.

**Diagramme de séquence d’acceptation de session.** Les participants sont l’utilisateur distant, l’interface client, le contrôleur REST des sessions, le service de session et le demandeur. L’utilisateur distant sélectionne les permissions et valide. L’interface envoie un `POST /api/sessions/{id}/approve`. Le service met à jour le statut de la session, persiste les permissions et notifie les deux parties via WebSocket. La séquence prépare le déclenchement de la signalisation WebRTC.

**Diagramme de séquence d’échange initial de signalisation WebRTC.** Les participants sont l’assistant, le `WebSocket Signaling` du serveur et le demandeur. L’assistant produit une SDP Offer et l’envoie au serveur via un message de type `offer`. Le serveur la relaie au demandeur. Le demandeur produit une SDP Answer et l’envoie en retour. Les deux pairs échangent ensuite des candidats ICE par messages successifs. Lorsque l’état ICE atteint « connected », la connexion temps réel est établie et les DataChannels peuvent être ouverts.

## 3.4 Mise en œuvre

### 3.4.1 Interface d’authentification

L’interface d’authentification, première fenêtre rencontrée par l’utilisateur, a été conçue pour respecter deux principes : la simplicité visuelle et la rigueur fonctionnelle. Elle se compose d’un en‑tête identifiant la plateforme, d’un sous‑titre indiquant le rôle de l’écran, d’un champ de saisie pour l’**adresse électronique**, d’un champ de saisie pour le **mot de passe**, d’un bouton primaire **« Se connecter »** et d’une zone discrète destinée à afficher les messages d’erreur. Les champs font l’objet d’une **validation côté client** : l’adresse doit respecter un format reconnu, le mot de passe doit comporter une longueur minimale ; le bouton de connexion ne devient actif qu’à partir du moment où les contraintes sont satisfaites. Les **erreurs serveur** (identifiants invalides, compte verrouillé, indisponibilité du service) sont restituées sous forme de messages clairs, sans révéler d’information sensible. À la **redirection** consécutive à une connexion réussie, l’utilisateur est dirigé vers l’interface principale de l’application, qui affiche immédiatement son état de connexion et l’identifiant de son poste.

### 3.4.2 Interface principale de l’application client

L’interface principale matérialise le caractère *client‑to‑client* de la plateforme. Elle est strictement identique sur tous les postes, et c’est l’usage qui détermine, au sein d’une session donnée, le rôle endossé par l’utilisateur.

Elle se compose d’un **en‑tête supérieur** affichant l’état de connexion à la plateforme — par exemple « En ligne », « Hors ligne » ou « Reconnexion en cours » —, le **nom de l’utilisateur** authentifié et l’**identifiant unique** du poste, présenté sous une forme facilement communicable. Un bouton permet de **démarrer ou arrêter le client**, c’est‑à‑dire de rendre le poste effectivement disponible ou de masquer sa présence.

Le corps de l’interface est organisé en plusieurs panneaux complémentaires. Un panneau de **connexion** permet de saisir l’identifiant d’un autre poste afin de **demander une assistance**. Un second panneau, le **chat**, est destiné aux échanges textuels avec le poste appairé pendant la session. Un troisième panneau gère le **transfert de fichiers**, avec sa liste des transferts en cours et son historique. Un quatrième panneau est dédié au **partage d’écran**, qui restitue le flux distant et expose les contrôles de qualité et d’interaction. Un cinquième panneau présente les **métriques système** du poste distant, en temps réel et sous forme de séries chronologiques courtes.

L’interface affiche également, dans une zone spécifique, l’**état de la session** courante et les **permissions accordées**, sous une forme visuelle compacte (par exemple une rangée d’icônes représentant le clavier/souris, le transfert de fichiers et l’IA). L’utilisateur a connaissance, à tout instant, des droits effectivement en vigueur, et le composant de session expose les actions de **fin de session**. Cette ergonomie, à la fois dense et lisible, vise à offrir une vision unifiée du contexte temps réel tout en respectant le principe selon lequel un utilisateur ne joue jamais un rôle figé : la même interface lui permet d’apporter ou de recevoir de l’assistance, selon la situation.

---

# CONCLUSION GÉNÉRALE ET PERSPECTIVES

Au terme de ce projet de fin d’études, il est possible de dresser un bilan riche d’enseignements, tant sur le plan technique que sur le plan méthodologique. La démarche entreprise a permis de concevoir et de développer, à partir d’une page blanche, une plateforme intelligente de support informatique à distance fondée sur une architecture résolument *client‑to‑client*. L’application desktop, identique sur tous les postes, autorise chaque utilisateur à se positionner alternativement en demandeur d’assistance ou en assistant, tandis qu’un serveur central Spring Boot orchestre l’authentification, la gestion des sessions et la signalisation WebRTC.

Sur le plan **fonctionnel**, les objectifs initiaux ont été atteints dans leur grande majorité. L’utilisateur peut s’authentifier, lancer l’application, voir sa présence enregistrée auprès du serveur, demander ou recevoir une session, définir des permissions explicites, partager son écran, autoriser le contrôle distant, échanger via le chat, transférer des fichiers, consulter les métriques système du poste appairé et bénéficier d’une assistance par intelligence artificielle pour appuyer le diagnostic. Sur le plan **technique**, l’ensemble du pipeline a été mis en place : capture d’écran native, encodage H.264, transport WebRTC sur DataChannel, signalisation WebSocket intégrée au backend, persistance des sessions, des messages, des transferts et des interactions IA dans une base MySQL conforme aux tables `users`, `agents`, `control_sessions`, `agent_metrics`, `chat_messages`, `file_transfer_logs` et `ai_sessions`.

Plusieurs **difficultés** ont jalonné la réalisation. La première a tenu à la maîtrise du protocole WebRTC, dont la mise en œuvre concrète, par‑delà la simplicité apparente des API, suppose une compréhension fine des candidats ICE, des cinématiques de négociation SDP et du rôle exact des serveurs STUN et TURN. La deuxième est venue de la performance du pipeline vidéo, qui a nécessité un travail d’optimisation pour maintenir une latence acceptable. La troisième a porté sur la **traversée des NAT** lors des tests inter‑réseaux, qui a exigé la configuration soignée d’un serveur TURN. Enfin, l’**intégration de l’IA** a soulevé la question délicate de l’équilibre entre l’automatisation utile et la maîtrise par l’utilisateur des actions exécutées en son nom.

Les **compétences acquises** au cours de ce stage de six mois sont nombreuses. Sur le plan technique, le projet a permis d’approfondir la programmation native en **Rust**, la conception d’applications desktop avec **Tauri**, le développement de services Spring Boot, la mise en œuvre du protocole **WebRTC**, l’usage avancé des **WebSockets**, la conception de bases de données relationnelles, l’architecture des systèmes temps réel et l’intégration d’un modèle d’IA multimodal via une API publique. Sur le plan méthodologique, il a permis d’apprivoiser le cadre **Scrum** dans un contexte individuel encadré, d’adopter une posture rigoureuse de modélisation **UML** et de mener un projet logiciel ambitieux de bout en bout.

Le projet conserve néanmoins certaines **limites**. Il cible aujourd’hui exclusivement Windows ; son IA repose sur une API externe ; sa scalabilité est conditionnée par les capacités du serveur de signalisation déployé ; et les tests de charge n’ont été réalisés que sur un nombre restreint de paires de clients.

Plusieurs **perspectives d’amélioration** se dessinent. Elles comprennent l’**amélioration de la qualité vidéo** par l’usage d’encodeurs matériels avancés et de schémas adaptatifs de bitrate ; l’**optimisation de la latence** par un réglage plus fin des stratégies de jitter buffer et de retransmission ; le **renforcement de la sécurité** par l’ajout d’une authentification multifacteur et d’une gestion granulaire des certificats ; l’**ajout d’un historique avancé** des sessions avec recherche et statistiques ; l’**amélioration de l’IA** par une intégration plus profonde avec le contexte applicatif et par la prise en charge de modèles locaux ; le **support multi‑plateforme** Linux et macOS, rendu accessible par le caractère portable de Tauri et Rust ; l’**amélioration du transfert de fichiers** par la reprise sur interruption et la vérification d’intégrité ; le **déploiement cloud complet** sur une infrastructure managée incluant TURN ; le **monitoring avancé** du serveur central et des sessions ; et l’**amélioration de la gestion des rôles dynamiques** entre demandeur et assistant, par exemple via l’inversion de rôle en cours de session.

Le présent projet constitue ainsi une base à la fois aboutie et extensible. Il démontre la viabilité technique d’une approche *client‑to‑client* pour le support informatique à distance et ouvre la voie à des évolutions concrètes, tant sur le plan fonctionnel que sur le plan industriel.

---

# WEBGRAPHIE

1. **WebRTC** — Documentation officielle et spécifications W3C. *WebRTC API*. https://webrtc.org — https://www.w3.org/TR/webrtc/
2. **WebRTC for the Curious** — Ouvrage en ligne de référence sur les internes de WebRTC. https://webrtcforthecurious.com
3. **Tauri** — Documentation officielle. *Build smaller, faster, and more secure desktop applications with a web frontend.* https://tauri.app
4. **Rust** — *The Rust Programming Language Book.* https://doc.rust-lang.org/book/
5. **Rust Async Book** — *Asynchronous Programming in Rust.* https://rust-lang.github.io/async-book/
6. **Spring Boot** — Documentation officielle. https://spring.io/projects/spring-boot
7. **Spring Security** — Documentation officielle. https://docs.spring.io/spring-security/reference/
8. **WebSocket** — *The WebSocket Protocol*, RFC 6455. https://www.rfc-editor.org/rfc/rfc6455
9. **STUN** — *Session Traversal Utilities for NAT*, RFC 5389/8489. https://www.rfc-editor.org/rfc/rfc8489
10. **TURN** — *Traversal Using Relays around NAT*, RFC 5766/8656. https://www.rfc-editor.org/rfc/rfc8656
11. **Coturn** — Implémentation open‑source de référence d’un serveur STUN/TURN. https://github.com/coturn/coturn
12. **Scrum Guide** — Version officielle de référence du cadre Scrum. https://scrumguides.org
13. **UML** — Spécification officielle du langage UML par l’OMG. https://www.omg.org/spec/UML/
14. **JWT** — *JSON Web Token*, RFC 7519. https://www.rfc-editor.org/rfc/rfc7519 — https://jwt.io
15. **MySQL** — Documentation officielle. https://dev.mysql.com/doc/
16. **Svelte** — Documentation officielle. https://svelte.dev/docs
17. **Google Gemini API** — Documentation officielle de l’API multimodale Gemini. https://ai.google.dev/gemini-api/docs
18. **MDN — WebRTC** — Référence développeur sur l’API WebRTC du navigateur. https://developer.mozilla.org/en-US/docs/Web/API/WebRTC_API
19. **OWASP** — *Application Security Verification Standard.* https://owasp.org/www-project-application-security-verification-standard/
20. **Microsoft Docs — Desktop Duplication API** — Documentation sur la capture d’écran via DXGI. https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/desktop-dup-api
