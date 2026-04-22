with source as (
    select * from {{ ref('raw_orders') }}
)

select
    id as order_id,
    user_id as customer_id,
    order_date,
    status
from source
