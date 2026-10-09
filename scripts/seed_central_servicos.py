"""Seed mínimo da Central de Serviços para uma base SUAP LOCAL de desenvolvimento.

Cria (de forma idempotente) um campus/setor, o servidor do usuário de teste, um atendente e o
catálogo mínimo (área, categoria, grupo de serviço, centro de atendimento, serviço, grupo de
atendimento e um artigo da base de conhecimento) para que `chamados open` consiga abrir chamados.

Reaproveita as factories do próprio SUAP (`rh.tests.factories`), como o `builders.py` da
central de serviços. NUNCA rode contra homologação ou produção.

Uso (container web do SUAP local):

    docker exec -i <container-web-do-suap> python manage.py shell < scripts/seed_central_servicos.py
"""

import os

from django.contrib.auth.models import Group
from django.db import transaction

from centralservicos.models import BaseConhecimento, CategoriaServico, CentroAtendimento, GrupoAtendimento, GrupoServico, Servico
from comum.models import AreaAtuacao
from rh.models import Setor
from rh.tests.factories import ServidorFactory, SetorFactory

MATRICULA = os.environ.get("SEED_MATRICULA", "2080882")
SETOR_SIGLA = "ZL"
PREFIXO = "Seed chamados-cli"


def obter_ou_criar_setor() -> Setor:
    setor = Setor.objects.filter(sigla=SETOR_SIGLA, codigo__isnull=True).first()
    return setor or SetorFactory(sigla=SETOR_SIGLA, nome="Campus Zona Leste (seed)")


with transaction.atomic():
    setor = obter_ou_criar_setor()
    servidor = ServidorFactory(matricula=MATRICULA, setor=setor)
    usuario = servidor.get_user()
    usuario.groups.add(Group.objects.get(name="Atendente da Central de Serviços"))

    area, _ = AreaAtuacao.objects.get_or_create(nome=f"{PREFIXO} - Área")
    categoria, _ = CategoriaServico.objects.get_or_create(nome=f"{PREFIXO} - Categoria", defaults={"area": area})
    grupo_servico, criado = GrupoServico.objects.get_or_create(
        nome=f"{PREFIXO} - Grupo de serviço", defaults={"detalhamento": "Grupo criado pelo seed de desenvolvimento."}
    )
    if criado:
        grupo_servico.categorias.add(categoria)

    centro, _ = CentroAtendimento.objects.get_or_create(
        nome=f"{PREFIXO} - Centro", area=area, defaults={"eh_local": True}
    )
    servico, criado = Servico.objects.get_or_create(
        nome=f"{PREFIXO} - Serviço de teste",
        defaults={"tipo": Servico.TIPO_REQUISICAO, "area": area, "grupo_servico": grupo_servico, "permite_anexos": True},
    )
    if criado:
        servico.centros_atendimento.add(centro)
    if not servico.permite_anexos:
        # O `open --attach` e o `download` precisam de um serviço que aceite anexos.
        servico.permite_anexos = True
        servico.save()

    grupo_atendimento, _ = GrupoAtendimento.objects.get_or_create(
        nome=f"{PREFIXO} - Grupo de atendimento",
        campus=setor.uo,
        centro_atendimento=centro,
    )
    grupo_atendimento.responsaveis.add(usuario)
    grupo_atendimento.atendentes.add(usuario)

    # O SUAP exige ao menos um artigo relacionado para resolver chamados.
    artigo, criado = BaseConhecimento.objects.get_or_create(
        titulo=f"{PREFIXO} - Artigo",
        defaults={"resumo": "Artigo criado pelo seed de desenvolvimento.", "area": area, "atualizado_por": usuario},
    )
    if criado:
        artigo.grupos_atendimento.add(grupo_atendimento)
        artigo.servicos.add(servico)

    print(f"SEED OK servico_id={servico.pk} campus_id={setor.uo_id} centro_id={centro.pk} usuario={usuario.username}")
